import { describe, expect, it } from 'vitest';
import {
  collectTurn,
} from '../../../src/providers/collect.js';
import { ProviderFailure, type ProviderStreamEvent } from '../../../src/providers/provider.js';

async function* streamOf(events: readonly ProviderStreamEvent[]): AsyncIterable<ProviderStreamEvent> {
  for (const event of events) yield event;
}

describe('collectTurn', () => {
  it('concatenates text deltas in order', async () => {
    const result = await collectTurn(
      streamOf([
        { type: 'text-delta', text: 'Hello ' },
        { type: 'text-delta', text: 'wor' },
        { type: 'text-delta', text: 'ld' },
        { type: 'completed', finishReason: 'stop', usage: { inputTokens: 1, outputTokens: 2 } },
      ]),
    );
    expect(result.text).toBe('Hello world');
    expect(result.finishReason).toBe('stop');
  });

  it('collects tool calls in arrival order', async () => {
    const result = await collectTurn(
      streamOf([
        { type: 'text-delta', text: 'thinking' },
        { type: 'tool-call', call: { id: 'a', name: 'first', arguments: { n: 1 } } },
        { type: 'tool-call', call: { id: 'b', name: 'second', arguments: { n: 2 } } },
        { type: 'completed', finishReason: 'tool-calls', usage: { inputTokens: 0, outputTokens: 0 } },
      ]),
    );
    expect(result.toolCalls.map((call) => call.id)).toEqual(['a', 'b']);
    expect(result.finishReason).toBe('tool-calls');
  });

  it('keeps the last usage event seen before completion', async () => {
    const result = await collectTurn(
      streamOf([
        { type: 'usage', usage: { inputTokens: 5, outputTokens: 5 } },
        { type: 'usage', usage: { inputTokens: 9, outputTokens: 7 } },
        { type: 'completed', finishReason: 'stop', usage: { inputTokens: 12, outputTokens: 8 } },
      ]),
    );
    expect(result.usage).toEqual({ inputTokens: 12, outputTokens: 8 });
  });

  it('uses completed usage over earlier usage events', async () => {
    const result = await collectTurn(
      streamOf([
        { type: 'usage', usage: { inputTokens: 3, outputTokens: 1 } },
        { type: 'completed', finishReason: 'stop', usage: { inputTokens: 4, outputTokens: 2 } },
      ]),
    );
    expect(result.usage).toEqual({ inputTokens: 4, outputTokens: 2 });
  });

  it('captures structured output events', async () => {
    const result = await collectTurn(
      streamOf([
        { type: 'text-delta', text: '{"kind":"intent"}' },
        { type: 'structured-output', name: 'intent-proposal', value: { kind: 'intent' } },
        { type: 'completed', finishReason: 'stop', usage: { inputTokens: 1, outputTokens: 1 } },
      ]),
    );
    expect(result.structuredOutput).toEqual({ name: 'intent-proposal', value: { kind: 'intent' } });
  });

  it("defaults finishReason to 'stop' and usage to zeros when the stream ends without completion", async () => {
    const result = await collectTurn(streamOf([{ type: 'text-delta', text: 'partial' }]));
    expect(result.finishReason).toBe('stop');
    expect(result.usage).toEqual({ inputTokens: 0, outputTokens: 0 });
    expect(result.text).toBe('partial');
  });

  it('throws the ProviderFailure carried by error events', async () => {
    const failure = new ProviderFailure('rate-limit', 'too many requests', true, 429);
    await expect(
      collectTurn(streamOf([{ type: 'text-delta', text: 'x' }, { type: 'error', error: failure }])),
    ).rejects.toBe(failure);
  });

  it('wraps non-ProviderFailure error events', async () => {
    await expect(
      collectTurn(
        streamOf([{ type: 'error', error: new Error('boom') as unknown as ProviderFailure }]),
      ),
    ).rejects.toMatchObject({ name: 'ProviderFailure', kind: 'unknown' });
  });
});
