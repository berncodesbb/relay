/**
 * Anthropic adapter.
 *
 * Translates Relay-owned requests into `@anthropic-ai/sdk` Messages API calls
 * and normalizes the raw stream events back into Relay-owned provider events.
 * Anthropic requires `max_tokens` (defaulted here), has no native JSON-schema
 * response mode (structured output is requested via a system instruction), and
 * requires alternating roles, so consecutive tool results are merged into a
 * single user message.
 */

import Anthropic from '@anthropic-ai/sdk';
import {
  ProviderFailure,
  type AssistantContent,
  type FinishReason,
  type ModelMessage,
  type ModelProvider,
  type ModelRequest,
  type ProviderStreamEvent,
  type ToolSpec,
} from './provider.js';
import { toProviderFailure } from './errors.js';

export type AnthropicProviderOptions = {
  /** API key; when omitted the SDK reads ANTHROPIC_API_KEY from the environment. */
  readonly apiKey?: string;
  readonly baseUrl?: string;
  /** Pre-built client override, used by offline tests. */
  readonly client?: Anthropic;
};

/** Anthropic requires max_tokens on every request. */
const DEFAULT_MAX_TOKENS = 8192;

type ToolBlockState = { kind: 'tool'; id: string; name: string; json: string };
type BlockState = { kind: 'text' } | ToolBlockState;

function mapStopReason(reason: string | null | undefined): FinishReason {
  if (reason === 'tool_use') return 'tool-calls';
  if (reason === 'max_tokens') return 'length';
  return 'stop';
}

type SdkMessageParam = Anthropic.MessageParam;
type SdkContentBlockParam = Anthropic.ContentBlockParam;

function assistantBlocks(content: readonly AssistantContent[]): SdkContentBlockParam[] {
  const blocks: SdkContentBlockParam[] = [];
  for (const block of content) {
    if (block.type === 'text') {
      if (block.text.length > 0) blocks.push({ type: 'text', text: block.text });
    } else {
      blocks.push({
        type: 'tool_use',
        id: block.call.id,
        name: block.call.name,
        input: block.call.arguments ?? {},
      });
    }
  }
  return blocks;
}

function toSdkMessages(messages: readonly ModelMessage[]): SdkMessageParam[] {
  const mapped: SdkMessageParam[] = [];
  for (const message of messages) {
    switch (message.role) {
      case 'user':
        mapped.push({ role: 'user', content: message.content });
        break;
      case 'assistant':
        mapped.push({ role: 'assistant', content: assistantBlocks(message.content) });
        break;
      case 'tool-result': {
        const block: Anthropic.ToolResultBlockParam = {
          type: 'tool_result',
          tool_use_id: message.toolCallId,
          content: message.content,
        };
        // Anthropic requires alternating roles: merge consecutive tool results
        // into one user message with multiple tool_result blocks.
        const previous = mapped[mapped.length - 1];
        if (previous !== undefined && previous.role === 'user' && Array.isArray(previous.content)) {
          previous.content = [...previous.content, block];
        } else {
          mapped.push({ role: 'user', content: [block] });
        }
        break;
      }
    }
  }
  return mapped;
}

function toSdkTools(tools: readonly ToolSpec[]): Anthropic.Messages.Tool[] {
  return tools.map((tool) => ({
    name: tool.name,
    description: tool.description,
    input_schema: { ...tool.parameters } as Anthropic.Messages.Tool.InputSchema,
  }));
}

/** Parse JSON that models sometimes wrap in markdown fences. */
function parseJsonish(text: string): unknown {
  const trimmed = text.trim();
  try {
    return JSON.parse(trimmed);
  } catch {
    const unfenced = trimmed.replace(/^```(?:json)?\s*/i, '').replace(/\s*```$/i, '');
    return JSON.parse(unfenced);
  }
}

export class AnthropicProvider implements ModelProvider {
  readonly name = 'anthropic' as const;
  readonly models: readonly string[] = ['claude-opus-5-5', 'claude-sonnet-5-5', 'claude-haiku-4-5'];

  private readonly client: Anthropic;

  constructor(options: AnthropicProviderOptions = {}) {
    this.client =
      options.client ??
      new Anthropic({
        // SDK-level retries are disabled so Relay's own retry policy owns backoff.
        maxRetries: 0,
        ...(options.apiKey !== undefined ? { apiKey: options.apiKey } : {}),
        ...(options.baseUrl !== undefined ? { baseURL: options.baseUrl } : {}),
      });
  }

  async *stream(request: ModelRequest): AsyncIterable<ProviderStreamEvent> {
    const sdkTools = request.tools !== undefined ? toSdkTools(request.tools) : [];
    let system = request.system ?? '';
    if (request.structuredOutput !== undefined) {
      // Anthropic has no JSON-schema response mode; instruct the model instead.
      const instruction =
        `Respond with ONLY a JSON object matching this schema: ${JSON.stringify(request.structuredOutput.schema)}`;
      system = system.length > 0 ? `${system}\n\n${instruction}` : instruction;
    }

    const params: Anthropic.Messages.MessageStreamParams = {
      model: request.model,
      max_tokens: request.maxOutputTokens ?? DEFAULT_MAX_TOKENS,
      messages: toSdkMessages(request.messages),
      ...(system.length > 0 ? { system } : {}),
      ...(sdkTools.length > 0 ? { tools: sdkTools } : {}),
      ...(request.temperature !== undefined ? { temperature: request.temperature } : {}),
    };

    let text = '';
    let inputTokens = 0;
    let outputTokens = 0;
    let stopReason: FinishReason | null = null;
    const blocks = new Map<number, BlockState>();
    /** Tool blocks in stream order; emitted when each block closes. */
    const closedToolBlocks: ToolBlockState[] = [];

    try {
      const options = request.signal !== undefined ? { signal: request.signal } : undefined;
      const stream = this.client.messages.stream(params, options);
      for await (const event of stream) {
        switch (event.type) {
          case 'message_start':
            inputTokens = event.message.usage?.input_tokens ?? 0;
            yield { type: 'usage', usage: { inputTokens, outputTokens } };
            break;
          case 'content_block_start': {
            const block = event.content_block;
            if (block.type === 'tool_use') {
              blocks.set(event.index, { kind: 'tool', id: block.id, name: block.name, json: '' });
            } else {
              blocks.set(event.index, { kind: 'text' });
            }
            break;
          }
          case 'content_block_delta': {
            const delta = event.delta;
            if (delta.type === 'text_delta') {
              text += delta.text;
              yield { type: 'text-delta', text: delta.text };
            } else if (delta.type === 'input_json_delta') {
              const block = blocks.get(event.index);
              if (block !== undefined && block.kind === 'tool') block.json += delta.partial_json;
            }
            break;
          }
          case 'content_block_stop': {
            const block = blocks.get(event.index);
            if (block !== undefined && block.kind === 'tool') closedToolBlocks.push(block);
            blocks.delete(event.index);
            break;
          }
          case 'message_delta':
            outputTokens = event.usage?.output_tokens ?? outputTokens;
            if (event.delta.stop_reason !== null && stopReason === null) {
              stopReason = mapStopReason(event.delta.stop_reason);
            }
            break;
          default:
            break;
        }
      }

      for (const block of closedToolBlocks) {
        let input: unknown;
        try {
          input = block.json.length > 0 ? JSON.parse(block.json) : {};
        } catch {
          input = block.json;
        }
        yield { type: 'tool-call', call: { id: block.id, name: block.name, arguments: input } };
      }

      if (request.structuredOutput !== undefined) {
        try {
          const value = parseJsonish(text);
          yield { type: 'structured-output', name: request.structuredOutput.name, value };
        } catch {
          yield {
            type: 'error',
            error: new ProviderFailure(
              'invalid-response',
              `Anthropic structured output was not valid JSON for "${request.structuredOutput.name}": ` +
                `${text.slice(0, 200)}`,
            ),
          };
          return;
        }
      }

      yield { type: 'completed', finishReason: stopReason ?? 'stop', usage: { inputTokens, outputTokens } };
    } catch (error) {
      const failure = toProviderFailure(error);
      if (request.signal?.aborted === true && failure.kind !== 'canceled') {
        yield {
          type: 'error',
          error: new ProviderFailure('canceled', 'request aborted before completion'),
        };
        return;
      }
      yield { type: 'error', error: failure };
    }
  }
}
