import { describe, expect, it } from 'vitest';
import type Anthropic from '@anthropic-ai/sdk';
import { AnthropicProvider } from '../../../src/providers/anthropic.js';
import { collectTurn } from '../../../src/providers/collect.js';
import { ProviderFailure, type ProviderStreamEvent } from '../../../src/providers/provider.js';

type RawEvent = Record<string, unknown>;

type Captured = { params?: unknown; options?: { signal?: AbortSignal } | undefined };

function makeClient(events: readonly RawEvent[], captured: Captured, error?: unknown): Anthropic {
  return {
    messages: {
      stream: (params: unknown, options?: { signal?: AbortSignal }) => {
        captured.params = params;
        captured.options = options;
        if (error !== undefined) throw error;
        return {
          [Symbol.asyncIterator]() {
            async function* iterate(): AsyncIterableIterator<RawEvent> {
              for (const event of events) yield event;
            }
            return iterate();
          },
        };
      },
    },
  } as unknown as Anthropic;
}

const REQUEST = {
  model: 'claude-sonnet-5-5',
  messages: [{ role: 'user' as const, content: 'inspect the repo' }],
  purpose: 'mode:technical-design',
};

function toolUseEvents(): RawEvent[] {
  return [
    { type: 'message_start', message: { id: 'm1', usage: { input_tokens: 10, output_tokens: 1 } } },
    { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
    { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'Let me check ' } },
    { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'that file.' } },
    { type: 'content_block_stop', index: 0 },
    {
      type: 'content_block_start',
      index: 1,
      content_block: { type: 'tool_use', id: 'tu_1', name: 'read-file', input: {} },
    },
    { type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json: '{"path":"sr' } },
    { type: 'content_block_delta', index: 1, delta: { type: 'input_json_delta', partial_json: 'c/app.ts"}' } },
    { type: 'content_block_stop', index: 1 },
    {
      type: 'message_delta',
      delta: { stop_reason: 'tool_use', stop_sequence: null },
      usage: { output_tokens: 7 },
    },
    { type: 'message_stop' },
  ];
}

describe('AnthropicProvider', () => {
  it('normalizes text, tool_use input fragments, usage, and stop reason', async () => {
    const captured: Captured = {};
    const provider = new AnthropicProvider({ client: makeClient(toolUseEvents(), captured) });
    const result = await collectTurn(provider.stream(REQUEST));

    expect(result.text).toBe('Let me check that file.');
    expect(result.toolCalls).toEqual([
      { id: 'tu_1', name: 'read-file', arguments: { path: 'src/app.ts' } },
    ]);
    expect(result.usage).toEqual({ inputTokens: 10, outputTokens: 7 });
    expect(result.finishReason).toBe('tool-calls');

    const params = captured.params as Record<string, unknown>;
    expect(params.model).toBe('claude-sonnet-5-5');
    expect(params.max_tokens).toBe(8192);
    expect(captured.options?.signal).toBeUndefined();
  });

  it('maps stop reasons end_turn and max_tokens', async () => {
    const endTurn = toolUseEvents().map((event) =>
      event.type === 'message_delta'
        ? { ...event, delta: { stop_reason: 'end_turn', stop_sequence: null } }
        : event,
    );
    const resultA = await collectTurn(
      new AnthropicProvider({ client: makeClient(endTurn, {}) }).stream(REQUEST),
    );
    expect(resultA.finishReason).toBe('stop');

    const maxTokens = endTurn.map((event) =>
      event.type === 'message_delta'
        ? { ...event, delta: { stop_reason: 'max_tokens', stop_sequence: null } }
        : event,
    );
    const resultB = await collectTurn(
      new AnthropicProvider({ client: makeClient(maxTokens, {}) }).stream(REQUEST),
    );
    expect(resultB.finishReason).toBe('length');
  });

  it('maps messages: tool results merge into one user turn, assistant calls replay as tool_use', async () => {
    const captured: Captured = {};
    const provider = new AnthropicProvider({
      client: makeClient(
        [
          { type: 'message_start', message: { id: 'm', usage: { input_tokens: 1, output_tokens: 1 } } },
          { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 1 } },
          { type: 'message_stop' },
        ],
        captured,
      ),
    });

    await collectTurn(
      provider.stream({
        model: 'claude-haiku-4-5',
        system: 'sys prompt',
        maxOutputTokens: 1000,
        temperature: 0.3,
        messages: [
          { role: 'user', content: 'check both' },
          {
            role: 'assistant',
            content: [
              { type: 'tool-call', call: { id: 't1', name: 'read-file', arguments: { path: 'a.ts' } } },
              { type: 'tool-call', call: { id: 't2', name: 'read-file', arguments: { path: 'b.ts' } } },
            ],
          },
          { role: 'tool-result', toolCallId: 't1', content: 'contents a' },
          { role: 'tool-result', toolCallId: 't2', content: 'contents b' },
        ],
        tools: [
          {
            name: 'read-file',
            description: 'Reads a file',
            parameters: { type: 'object', properties: { path: { type: 'string' } } },
          },
        ],
      }),
    );

    const params = captured.params as {
      system: string;
      max_tokens: number;
      temperature: number;
      messages: Array<Record<string, unknown>>;
      tools: Array<Record<string, unknown>>;
    };
    expect(params.system).toBe('sys prompt');
    expect(params.max_tokens).toBe(1000);
    expect(params.temperature).toBe(0.3);
    expect(params.messages).toEqual([
      { role: 'user', content: 'check both' },
      {
        role: 'assistant',
        content: [
          { type: 'tool_use', id: 't1', name: 'read-file', input: { path: 'a.ts' } },
          { type: 'tool_use', id: 't2', name: 'read-file', input: { path: 'b.ts' } },
        ],
      },
      {
        role: 'user',
        content: [
          { type: 'tool_result', tool_use_id: 't1', content: 'contents a' },
          { type: 'tool_result', tool_use_id: 't2', content: 'contents b' },
        ],
      },
    ]);
    expect(params.tools).toEqual([
      {
        name: 'read-file',
        description: 'Reads a file',
        input_schema: { type: 'object', properties: { path: { type: 'string' } } },
      },
    ]);
  });

  it('prepends a JSON schema instruction for structured output and parses the reply', async () => {
    const captured: Captured = {};
    const events: RawEvent[] = [
      { type: 'message_start', message: { id: 'm', usage: { input_tokens: 4, output_tokens: 2 } } },
      { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
      { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: '{"mode":"discovery"}' } },
      { type: 'content_block_stop', index: 0 },
      { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 3 } },
      { type: 'message_stop' },
    ];
    const provider = new AnthropicProvider({ client: makeClient(events, captured) });

    const result = await collectTurn(
      provider.stream({
        model: 'claude-sonnet-5-5',
        system: 'classifier',
        messages: [{ role: 'user', content: 'classify' }],
        structuredOutput: { name: 'intent-proposal', schema: { type: 'object' } },
      }),
    );

    const params = captured.params as { system: string };
    expect(params.system).toContain('classifier');
    expect(params.system).toContain('Respond with ONLY a JSON object matching this schema:');
    expect(params.system).toContain('"type":"object"');
    expect(result.structuredOutput).toEqual({
      name: 'intent-proposal',
      value: { mode: 'discovery' },
    });
    // Structured output is emitted before completion so consumers can rely on order.
    const ordered = await (async () => {
      const collected: ProviderStreamEvent[] = [];
      const client = makeClient(events, {});
      for await (const event of new AnthropicProvider({ client }).stream({
        model: 'claude-sonnet-5-5',
        messages: [{ role: 'user', content: 'classify' }],
        structuredOutput: { name: 'intent-proposal', schema: { type: 'object' } },
      })) {
        collected.push(event);
      }
      return collected.map((event) => event.type);
    })();
    expect(ordered.indexOf('structured-output')).toBeLessThan(ordered.indexOf('completed'));
  });

  it('emits invalid-response when the structured reply is not JSON', async () => {
    const events: RawEvent[] = [
      { type: 'message_start', message: { id: 'm', usage: { input_tokens: 1, output_tokens: 1 } } },
      { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
      { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'definitely { not json' } },
      { type: 'content_block_stop', index: 0 },
      { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 2 } },
      { type: 'message_stop' },
    ];
    const provider = new AnthropicProvider({ client: makeClient(events, {}) });

    await expect(
      collectTurn(
        provider.stream({
          model: 'claude-sonnet-5-5',
          messages: [{ role: 'user', content: 'classify' }],
          structuredOutput: { name: 'intent-proposal', schema: { type: 'object' } },
        }),
      ),
    ).rejects.toMatchObject({
      name: 'ProviderFailure',
      kind: 'invalid-response',
      retryable: false,
    });
  });

  it('falls back to the raw JSON string when tool input cannot be parsed', async () => {
    let replaced = false;
    const broken = toolUseEvents().map((event) => {
      if (event.type === 'content_block_delta' && event.index === 1 && !replaced) {
        replaced = true;
        return { ...event, delta: { type: 'input_json_delta', partial_json: '{broken' } };
      }
      if (event.type === 'content_block_delta' && event.index === 1) {
        return { ...event, delta: { type: 'input_json_delta', partial_json: '' } };
      }
      return event;
    });
    const result = await collectTurn(
      new AnthropicProvider({ client: makeClient(broken, {}) }).stream(REQUEST),
    );
    expect(result.toolCalls[0]?.arguments).toBe('{broken');
  });

  it('maps a 429 SDK error to a retryable rate-limit failure', async () => {
    const error = new Error('rate limited');
    (error as Error & { status?: number }).status = 429;
    const provider = new AnthropicProvider({ client: makeClient([], {}, error) });

    await expect(collectTurn(provider.stream(REQUEST))).rejects.toMatchObject({
      name: 'ProviderFailure',
      kind: 'rate-limit',
      retryable: true,
      status: 429,
    });
  });

  it('reports cancellation when the signal aborts mid-stream', async () => {
    const controller = new AbortController();
    const provider = new AnthropicProvider({
      client: makeClient([], {}, new DOMException('aborted', 'AbortError')),
    });
    controller.abort('user interrupt');
    await expect(
      collectTurn(provider.stream({ ...REQUEST, signal: controller.signal })),
    ).rejects.toMatchObject({ kind: 'canceled', retryable: false });
  });

  it('exposes anthropic metadata without leaking SDK types', () => {
    const provider = new AnthropicProvider({ client: makeClient([], {}) });
    expect(provider.name).toBe('anthropic');
    expect(provider.models).toEqual(['claude-opus-5-5', 'claude-sonnet-5-5', 'claude-haiku-4-5']);
    expect(provider.models).not.toContain(ProviderFailure);
  });
});
