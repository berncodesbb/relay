import { describe, expect, it } from 'vitest';
import type OpenAI from 'openai';
import { OpenAIProvider } from '../../../src/providers/openai.js';
import { collectTurn } from '../../../src/providers/collect.js';
import { ProviderFailure, type ProviderStreamEvent } from '../../../src/providers/provider.js';

type Chunk = Record<string, unknown>;

type Captured = { params?: unknown; options?: { signal?: AbortSignal } | undefined };

function makeClient(chunks: readonly Chunk[], captured: Captured, error?: unknown): OpenAI {
  return {
    chat: {
      completions: {
        create: async (params: unknown, options?: { signal?: AbortSignal }) => {
          captured.params = params;
          captured.options = options;
          if (error !== undefined) throw error;
          async function* iterate(): AsyncIterableIterator<Chunk> {
            for (const chunk of chunks) yield chunk;
          }
          return iterate();
        },
      },
    },
  } as unknown as OpenAI;
}

async function allEvents(iterable: AsyncIterable<ProviderStreamEvent>): Promise<ProviderStreamEvent[]> {
  const events: ProviderStreamEvent[] = [];
  for await (const event of iterable) events.push(event);
  return events;
}

function textChunk(text: string): Chunk {
  return {
    id: 'c1',
    choices: [{ index: 0, delta: { role: 'assistant', content: text }, finish_reason: null }],
  };
}

function toolCallChunk(index: number, fragment: Chunk): Chunk {
  return {
    id: 'c1',
    choices: [{ index: 0, delta: { tool_calls: [{ index, ...fragment }] }, finish_reason: null }],
  };
}

function finishChunk(reason: string): Chunk {
  return { id: 'c1', choices: [{ index: 0, delta: {}, finish_reason: reason }] };
}

function usageChunk(prompt: number, completion: number): Chunk {
  return { id: 'c1', choices: [], usage: { prompt_tokens: prompt, completion_tokens: completion } };
}

const REQUEST = {
  model: 'gpt-5.2',
  messages: [{ role: 'user' as const, content: 'list the files' }],
  purpose: 'mode:planning',
};

describe('OpenAIProvider', () => {
  it('normalizes text deltas, split tool-call fragments, usage, and finish reason', async () => {
    const captured: Captured = {};
    const client = makeClient(
      [
        textChunk('Sure, '),
        textChunk('checking.'),
        toolCallChunk(0, { id: 'call_a', function: { name: 'list-files', arguments: '{"path":"sr' } }),
        toolCallChunk(0, { function: { arguments: 'c"}' } }),
        finishChunk('tool_calls'),
        usageChunk(120, 45),
      ],
      captured,
    );
    const provider = new OpenAIProvider({ client });
    const result = await collectTurn(provider.stream(REQUEST));

    expect(result.text).toBe('Sure, checking.');
    expect(result.toolCalls).toEqual([
      { id: 'call_a', name: 'list-files', arguments: { path: 'src' } },
    ]);
    expect(result.usage).toEqual({ inputTokens: 120, outputTokens: 45 });
    expect(result.finishReason).toBe('tool-calls');

    const params = captured.params as Record<string, unknown>;
    expect(params.stream).toBe(true);
    expect(params.stream_options).toEqual({ include_usage: true });
    expect(captured.options?.signal).toBeUndefined();
  });

  it('replays assistant tool calls and tool results in request messages', async () => {
    const captured: Captured = {};
    const client = makeClient([finishChunk('stop'), usageChunk(1, 1)], captured);
    const provider = new OpenAIProvider({ client });

    await collectTurn(
      provider.stream({
        model: 'gpt-4o',
        system: 'be brief',
        messages: [
          { role: 'user', content: 'read app.ts' },
          {
            role: 'assistant',
            content: [
              { type: 'text', text: 'reading' },
              { type: 'tool-call', call: { id: 'call_1', name: 'read-file', arguments: { path: 'app.ts' } } },
            ],
          },
          { role: 'tool-result', toolCallId: 'call_1', content: 'file body' },
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
      messages: Array<Record<string, unknown>>;
      tools: Array<Record<string, unknown>>;
    };
    expect(params.messages).toEqual([
      { role: 'developer', content: 'be brief' },
      { role: 'user', content: 'read app.ts' },
      {
        role: 'assistant',
        content: 'reading',
        tool_calls: [
          {
            id: 'call_1',
            type: 'function',
            function: { name: 'read-file', arguments: '{"path":"app.ts"}' },
          },
        ],
      },
      { role: 'tool', tool_call_id: 'call_1', content: 'file body' },
    ]);
    expect(params.tools).toEqual([
      {
        type: 'function',
        function: {
          name: 'read-file',
          description: 'Reads a file',
          parameters: { type: 'object', properties: { path: { type: 'string' } } },
        },
      },
    ]);
  });

  it('uses max_completion_tokens for gpt-5 models and max_tokens for older ones', async () => {
    const capturedA: Captured = {};
    await collectTurn(
      new OpenAIProvider({ client: makeClient([finishChunk('stop')], capturedA) }).stream({
        model: 'gpt-5-mini',
        messages: [{ role: 'user', content: 'hi' }],
        maxOutputTokens: 512,
      }),
    );
    expect((capturedA.params as Record<string, unknown>).max_completion_tokens).toBe(512);
    expect((capturedA.params as Record<string, unknown>).max_tokens).toBeUndefined();

    const capturedB: Captured = {};
    await collectTurn(
      new OpenAIProvider({ client: makeClient([finishChunk('stop')], capturedB) }).stream({
        model: 'gpt-4o',
        messages: [{ role: 'user', content: 'hi' }],
        maxOutputTokens: 256,
        temperature: 0.2,
      }),
    );
    expect((capturedB.params as Record<string, unknown>).max_tokens).toBe(256);
    expect((capturedB.params as Record<string, unknown>).temperature).toBe(0.2);
  });

  it('requests json_schema response format and emits parsed structured output', async () => {
    const captured: Captured = {};
    const client = makeClient(
      [textChunk('{"kind":"intent","mode":"discovery"}'), finishChunk('stop'), usageChunk(9, 9)],
      captured,
    );
    const provider = new OpenAIProvider({ client });
    const result = await collectTurn(
      provider.stream({
        model: 'gpt-4.1',
        messages: [{ role: 'user', content: 'classify' }],
        structuredOutput: { name: 'intent-proposal', schema: { type: 'object' } },
      }),
    );

    const responseFormat = (captured.params as { response_format: Record<string, unknown> })
      .response_format;
    expect(responseFormat.type).toBe('json_schema');
    expect(responseFormat.json_schema).toEqual({
      name: 'intent-proposal',
      schema: { type: 'object' },
      strict: false,
    });
    expect(result.structuredOutput).toEqual({
      name: 'intent-proposal',
      value: { kind: 'intent', mode: 'discovery' },
    });
  });

  it('emits an invalid-response error when structured text is not JSON', async () => {
    const client = makeClient([textChunk('not json at all'), finishChunk('stop')], {});
    const provider = new OpenAIProvider({ client });
    const events = await allEvents(
      provider.stream({
        model: 'gpt-4.1',
        messages: [{ role: 'user', content: 'classify' }],
        structuredOutput: { name: 'intent-proposal', schema: { type: 'object' } },
      }),
    );

    const error = events.find((event) => event.type === 'error');
    expect(error).toBeDefined();
    if (error?.type === 'error') {
      expect(error.error).toBeInstanceOf(ProviderFailure);
      expect(error.error.kind).toBe('invalid-response');
      expect(error.error.retryable).toBe(false);
      expect(error.error.message).toContain('not json at all');
    }
    expect(events.some((event) => event.type === 'completed')).toBe(false);
  });

  it('falls back to the raw string when tool arguments are not valid JSON', async () => {
    const client = makeClient(
      [
        toolCallChunk(0, { id: 'call_b', function: { name: 'odd', arguments: '{oops' } }),
        finishChunk('tool_calls'),
      ],
      {},
    );
    const result = await collectTurn(new OpenAIProvider({ client }).stream(REQUEST));
    expect(result.toolCalls[0]?.arguments).toBe('{oops');
  });

  it('maps HTTP 401 SDK errors to a non-retryable auth failure event', async () => {
    const error = new Error('Incorrect API key provided');
    (error as Error & { status?: number }).status = 401;
    const client = makeClient([], {}, error);
    const provider = new OpenAIProvider({ client });

    await expect(collectTurn(provider.stream(REQUEST))).rejects.toMatchObject({
      name: 'ProviderFailure',
      kind: 'auth',
      retryable: false,
      status: 401,
    });
  });

  it('maps thrown AbortError to a canceled failure and forwards the signal', async () => {
    const captured: Captured = {};
    const controller = new AbortController();
    controller.abort('user pressed ctrl-c');
    const client = makeClient([], captured, new DOMException('aborted', 'AbortError'));
    const provider = new OpenAIProvider({ client });

    await expect(
      collectTurn(provider.stream({ ...REQUEST, signal: controller.signal })),
    ).rejects.toMatchObject({ kind: 'canceled', retryable: false });
    expect(captured.options?.signal?.aborted).toBe(true);
  });

  it('reports its name and supported models', () => {
    const provider = new OpenAIProvider({ client: makeClient([], {}) });
    expect(provider.name).toBe('openai');
    expect(provider.models).toContain('gpt-5.2');
    expect(provider.models).toContain('gpt-4o');
  });
});
