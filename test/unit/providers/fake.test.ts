import { describe, expect, it } from 'vitest';
import { FakeProvider, createEchoFakeProvider } from '../../../src/providers/fake.js';
import { collectTurn } from '../../../src/providers/collect.js';

const BASE = { model: 'fake-standard', messages: [{ role: 'user' as const, content: 'hello' }] };

describe('FakeProvider', () => {
  it('routes by exact purpose string and regex', async () => {
    const provider = new FakeProvider([
      { match: { purpose: 'intent' }, response: { text: 'intent response' } },
      { match: { purpose: /^mode:/ }, response: { text: 'mode response' } },
    ]);

    const intent = await collectTurn(provider.stream({ ...BASE, purpose: 'intent' }));
    const mode = await collectTurn(provider.stream({ ...BASE, purpose: 'mode:discovery' }));
    expect(intent.text).toBe('intent response');
    expect(mode.text).toBe('mode response');
    expect(provider.callsByPurpose).toEqual({ intent: 1, 'mode:discovery': 1 });
  });

  it('matches model and minCall constraints in order', async () => {
    const provider = new FakeProvider([
      { match: { model: 'fake-mini' }, response: { text: 'from mini' } },
      { match: { purpose: 'p', minCall: 2 }, response: { text: 'second-plus call' } },
    ], { text: 'fallback' });

    const first = await collectTurn(provider.stream({ ...BASE, purpose: 'p' }));
    const second = await collectTurn(provider.stream({ ...BASE, purpose: 'p' }));
    const third = await collectTurn(provider.stream({ ...BASE, purpose: 'p', model: 'fake-mini' }));

    expect(first.text).toBe('fallback');
    expect(second.text).toBe('second-plus call');
    expect(third.text).toBe('from mini');
    expect(provider.callsByPurpose['p']).toBe(3);
  });

  it('scopes steps to a call window with maxCall', async () => {
    const provider = new FakeProvider([
      { match: { purpose: 'seq', maxCall: 1 }, response: { text: 'first only' } },
      { match: { purpose: 'seq', minCall: 2, maxCall: 2 }, response: { text: 'second only' } },
    ], { text: 'later' });

    const results: string[] = [];
    for (let i = 0; i < 3; i += 1) {
      const turn = await collectTurn(provider.stream({ ...BASE, purpose: 'seq' }));
      results.push(turn.text);
    }
    expect(results).toEqual(['first only', 'second only', 'later']);
  });

  it('emits tool calls with unique ids, default usage, and tool-calls finish reason', async () => {
    const provider = new FakeProvider(
      [],
      {
        toolCalls: [
          { name: 'read-file', args: { path: 'a.ts' } },
          { name: 'read-file', args: { path: 'b.ts' } },
        ],
      },
    );
    const result = await collectTurn(provider.stream({ ...BASE, purpose: 'x' }));
    expect(result.toolCalls.map((call) => call.name)).toEqual(['read-file', 'read-file']);
    expect(new Set(result.toolCalls.map((call) => call.id)).size).toBe(2);
    expect(result.finishReason).toBe('tool-calls');
    expect(result.usage).toEqual({ inputTokens: 100, outputTokens: 50 });
  });

  it('emits structured output only when the request asks for it', async () => {
    const provider = new FakeProvider([], { structuredOutput: { mode: 'planning' } });

    const without = await collectTurn(provider.stream({ ...BASE, purpose: 'x' }));
    expect(without.structuredOutput).toBeNull();

    const withSchema = await collectTurn(
      provider.stream({
        ...BASE,
        purpose: 'x',
        structuredOutput: { name: 'intent-proposal', schema: { type: 'object' } },
      }),
    );
    expect(withSchema.structuredOutput).toEqual({
      name: 'intent-proposal',
      value: { mode: 'planning' },
    });
  });

  it('fails failTimes times with the scripted error, then succeeds', async () => {
    const provider = new FakeProvider([
      {
        match: { purpose: 'flaky' },
        response: {
          text: 'finally ok',
          error: { kind: 'server', retryable: true, message: 'boom', failTimes: 2 },
        },
      },
    ]);

    await expect(collectTurn(provider.stream({ ...BASE, purpose: 'flaky' }))).rejects.toMatchObject({
      kind: 'server',
      retryable: true,
      message: 'boom',
    });
    await expect(collectTurn(provider.stream({ ...BASE, purpose: 'flaky' }))).rejects.toMatchObject({
      kind: 'server',
    });
    const third = await collectTurn(provider.stream({ ...BASE, purpose: 'flaky' }));
    expect(third.text).toBe('finally ok');
    expect(provider.callsByPurpose['flaky']).toBe(3);
  });

  it('records a call log with purpose, model, and response text', async () => {
    const provider = new FakeProvider([
      { match: { purpose: 'a' }, response: { text: 'alpha' } },
    ]);
    await collectTurn(provider.stream({ ...BASE, purpose: 'a' }));
    await collectTurn(provider.stream({ ...BASE, purpose: 'a', model: 'fake-mini' }));
    expect(provider.callLog).toEqual([
      { purpose: 'a', model: 'fake-standard', text: 'alpha' },
      { purpose: 'a', model: 'fake-mini', text: 'alpha' },
    ]);
  });

  it('cancels during delayMs when the signal aborts', async () => {
    const provider = new FakeProvider([], { text: 'slow', delayMs: 10_000 });
    const controller = new AbortController();
    const stream = provider.stream({ ...BASE, purpose: 'x', signal: controller.signal });
    const pending = collectTurn(stream);
    setTimeout(() => controller.abort('user'), 10);
    await expect(pending).rejects.toMatchObject({
      kind: 'canceled',
      retryable: false,
    });
  });

  it('splits text across multiple deltas so folding is exercised', async () => {
    const provider = new FakeProvider([], { text: 'abcdefghij' });
    const deltas: string[] = [];
    for await (const event of provider.stream({ ...BASE, purpose: 'x' })) {
      if (event.type === 'text-delta') deltas.push(event.text);
    }
    expect(deltas.length).toBeGreaterThanOrEqual(2);
    expect(deltas.join('')).toBe('abcdefghij');
  });

  it('echoes the last user message when no step matches', async () => {
    const provider = new FakeProvider([{ match: { purpose: 'other' }, response: { text: 'nope' } }]);
    const result = await collectTurn(
      provider.stream({
        ...BASE,
        purpose: 'x',
        messages: [
          { role: 'user', content: 'first' },
          { role: 'assistant', content: [{ type: 'text', text: 'middle' }] },
          { role: 'user', content: 'last question' },
        ],
      }),
    );
    expect(result.text).toBe('last question');

    const echo = createEchoFakeProvider();
    const echoed = await collectTurn(echo.stream({ ...BASE, purpose: 'y' }));
    expect(echoed.text).toBe('hello');
  });

  it('honors scripted finish reason and usage overrides', async () => {
    const provider = new FakeProvider([], {
      text: 'trunc',
      finishReason: 'length',
      usage: { inputTokens: 5, outputTokens: 6 },
    });
    const result = await collectTurn(provider.stream({ ...BASE, purpose: 'x' }));
    expect(result.finishReason).toBe('length');
    expect(result.usage).toEqual({ inputTokens: 5, outputTokens: 6 });
  });
});
