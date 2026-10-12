/**
 * OpenAI adapter.
 *
 * Translates Relay-owned requests into `openai` SDK chat-completion calls and
 * normalizes streamed chunks back into Relay-owned provider events. No SDK
 * type appears in this module's public API; errors pass through
 * `toProviderFailure` so headers and bodies never leak.
 */

import OpenAI from 'openai';
import {
  ProviderFailure,
  type FinishReason,
  type ModelMessage,
  type ModelProvider,
  type ModelRequest,
  type ProviderStreamEvent,
  type ToolCall,
  type ToolSpec,
} from './provider.js';
import { toProviderFailure } from './errors.js';

export type OpenAIProviderOptions = {
  /** API key; when omitted the SDK reads OPENAI_API_KEY from the environment. */
  readonly apiKey?: string;
  readonly baseUrl?: string;
  /** Pre-built client override, used by offline tests. */
  readonly client?: OpenAI;
};

type AccumulatedToolCall = { id: string; name: string; args: string };

/** Models that only accept `max_completion_tokens` (newer reasoning family). */
function usesCompletionTokens(model: string): boolean {
  return /^(gpt-5|o\d)/.test(model);
}

function mapFinishReason(reason: string | null | undefined): FinishReason {
  if (reason === 'tool_calls' || reason === 'function_call') return 'tool-calls';
  if (reason === 'length') return 'length';
  return 'stop';
}

function toSdkMessages(messages: readonly ModelMessage[]): OpenAI.Chat.Completions.ChatCompletionMessageParam[] {
  const mapped: OpenAI.Chat.Completions.ChatCompletionMessageParam[] = [];
  for (const message of messages) {
    switch (message.role) {
      case 'user':
        mapped.push({ role: 'user', content: message.content });
        break;
      case 'assistant': {
        let text = '';
        const toolCalls: OpenAI.Chat.Completions.ChatCompletionMessageToolCall[] = [];
        for (const block of message.content) {
          if (block.type === 'text') {
            text += block.text;
          } else {
            toolCalls.push({
              id: block.call.id,
              type: 'function',
              function: {
                name: block.call.name,
                arguments: JSON.stringify(block.call.arguments ?? null),
              },
            });
          }
        }
        mapped.push({
          role: 'assistant',
          content: text.length > 0 ? text : null,
          ...(toolCalls.length > 0 ? { tool_calls: toolCalls } : {}),
        });
        break;
      }
      case 'tool-result':
        mapped.push({ role: 'tool', tool_call_id: message.toolCallId, content: message.content });
        break;
    }
  }
  return mapped;
}

function toSdkTools(tools: readonly ToolSpec[]): OpenAI.Chat.Completions.ChatCompletionTool[] {
  return tools.map((tool) => ({
    type: 'function' as const,
    function: {
      name: tool.name,
      description: tool.description,
      parameters: { ...tool.parameters } as Record<string, unknown>,
    },
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

export class OpenAIProvider implements ModelProvider {
  readonly name = 'openai' as const;
  readonly models: readonly string[] = ['gpt-5.2', 'gpt-5-mini', 'gpt-4.1', 'gpt-4o'];

  private readonly client: OpenAI;

  constructor(options: OpenAIProviderOptions = {}) {
    this.client =
      options.client ??
      new OpenAI({
        // SDK-level retries are disabled so Relay's own retry policy owns backoff.
        maxRetries: 0,
        ...(options.apiKey !== undefined ? { apiKey: options.apiKey } : {}),
        ...(options.baseUrl !== undefined ? { baseURL: options.baseUrl } : {}),
      });
  }

  async *stream(request: ModelRequest): AsyncIterable<ProviderStreamEvent> {
    const sdkTools = request.tools !== undefined ? toSdkTools(request.tools) : [];
    const params: OpenAI.Chat.Completions.ChatCompletionCreateParamsStreaming = {
      model: request.model,
      messages: toSdkMessages(request.messages),
      stream: true,
      stream_options: { include_usage: true },
      ...(sdkTools.length > 0 ? { tools: sdkTools } : {}),
    };
    // The system prompt becomes a leading developer message.
    if (request.system !== undefined) {
      params.messages = [
        { role: 'developer', content: request.system },
        ...params.messages,
      ];
    }
    if (request.temperature !== undefined) params.temperature = request.temperature;
    if (request.maxOutputTokens !== undefined) {
      if (usesCompletionTokens(request.model)) {
        params.max_completion_tokens = request.maxOutputTokens;
      } else {
        params.max_tokens = request.maxOutputTokens;
      }
    }
    const responseFormat: OpenAI.Chat.Completions.ChatCompletionCreateParamsStreaming['response_format'] =
      request.structuredOutput !== undefined
        ? {
            type: 'json_schema',
            json_schema: {
              name: request.structuredOutput.name,
              schema: { ...request.structuredOutput.schema } as Record<string, unknown>,
              strict: false,
            },
          }
        : undefined;
    if (responseFormat !== undefined) {
      params.response_format = responseFormat;
    }

    let text = '';
    let lastUsage: { inputTokens: number; outputTokens: number } | null = null;
    let finishReason: FinishReason | null = null;
    const toolAccumulators: AccumulatedToolCall[] = [];
    const byIndex = new Map<number, AccumulatedToolCall>();

    try {
      const options =
        request.signal !== undefined ? { signal: request.signal } : undefined;
      const stream = await this.client.chat.completions.create(params, options);
      for await (const chunk of stream) {
        if (chunk.usage !== null && chunk.usage !== undefined) {
          lastUsage = {
            inputTokens: chunk.usage.prompt_tokens,
            outputTokens: chunk.usage.completion_tokens,
          };
          yield { type: 'usage', usage: lastUsage };
        }
        const choice = chunk.choices[0];
        if (choice === undefined) continue;
        const content = choice.delta?.content;
        if (typeof content === 'string' && content.length > 0) {
          text += content;
          yield { type: 'text-delta', text: content };
        }
        for (const fragment of choice.delta?.tool_calls ?? []) {
          let acc = byIndex.get(fragment.index);
          if (acc === undefined) {
            acc = { id: '', name: '', args: '' };
            byIndex.set(fragment.index, acc);
            toolAccumulators.push(acc);
          }
          if (typeof fragment.id === 'string' && fragment.id.length > 0) acc.id = fragment.id;
          if (typeof fragment.function?.name === 'string') acc.name += fragment.function.name;
          if (typeof fragment.function?.arguments === 'string') acc.args += fragment.function.arguments;
        }
        if (choice.finish_reason !== null && finishReason === null) {
          finishReason = mapFinishReason(choice.finish_reason);
        }
      }

      for (const acc of toolAccumulators) {
        const call: ToolCall = {
          id: acc.id.length > 0 ? acc.id : `call_${toolAccumulators.indexOf(acc)}`,
          name: acc.name,
          arguments: parseToolArguments(acc.args),
        };
        yield { type: 'tool-call', call };
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
              `OpenAI structured output was not valid JSON for "${request.structuredOutput.name}": ` +
                `${text.slice(0, 200)}`,
            ),
          };
          return;
        }
      }

      yield {
        type: 'completed',
        finishReason: finishReason ?? 'stop',
        usage: lastUsage ?? { inputTokens: 0, outputTokens: 0 },
      };
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

/** Tool arguments are streamed as JSON text; fall back to the raw string. */
function parseToolArguments(args: string): unknown {
  if (args.length === 0) return {};
  try {
    return JSON.parse(args);
  } catch {
    return args;
  }
}
