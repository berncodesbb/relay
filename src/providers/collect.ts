/**
 * Fold a provider event stream into a single model turn result.
 *
 * The agent loop streams events for live rendering and then needs one
 * aggregated value to decide what happened. `collectTurn` is that fold: it
 * concatenates text deltas, gathers tool calls in arrival order, keeps the
 * last reported usage, captures structured output, and converts `error`
 * events into thrown `ProviderFailure`s.
 */

import { ProviderFailure, type ModelTurnResult, type ProviderStreamEvent } from './provider.js';

const EMPTY_USAGE = { inputTokens: 0, outputTokens: 0 } as const;

/** Aggregate one provider turn from its stream of events. */
export async function collectTurn(stream: AsyncIterable<ProviderStreamEvent>): Promise<ModelTurnResult> {
  let text = '';
  const toolCalls: ModelTurnResult['toolCalls'][number][] = [];
  let usage: ModelTurnResult['usage'] | null = null;
  let structuredOutput: ModelTurnResult['structuredOutput'] = null;
  let finishReason: ModelTurnResult['finishReason'] | null = null;

  for await (const event of stream) {
    switch (event.type) {
      case 'text-delta':
        text += event.text;
        break;
      case 'tool-call':
        toolCalls.push(event.call);
        break;
      case 'usage':
        usage = event.usage;
        break;
      case 'structured-output':
        structuredOutput = { name: event.name, value: event.value };
        break;
      case 'completed': {
        finishReason = event.finishReason;
        // The completed event carries the authoritative final usage when present.
        usage = event.usage ?? usage;
        break;
      }
      case 'error':
        throw event.error instanceof ProviderFailure
          ? event.error
          : new ProviderFailure('unknown', 'provider stream failed');
    }
  }

  return {
    text,
    toolCalls,
    structuredOutput,
    finishReason: finishReason ?? 'stop',
    usage: usage ?? EMPTY_USAGE,
  };
}
