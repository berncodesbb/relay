/**
 * Deterministic in-process provider for tests and demos.
 *
 * The fake provider never touches the network. Callers script per-purpose
 * responses (with optional ordered failures for retry tests); unmatched calls
 * fall back to a simple echo. It records a call log so tests can assert
 * routing, retries, and cancellation behavior deterministically.
 */

import {
  ProviderFailure,
  type FinishReason,
  type ModelProvider,
  type ModelRequest,
  type ProviderErrorKind,
  type ProviderStreamEvent,
  type TokenUsage,
} from './provider.js';

export type FakeResponse = {
  readonly text?: string;
  readonly toolCalls?: readonly { readonly name: string; readonly args: unknown }[];
  readonly structuredOutput?: unknown;
  readonly finishReason?: FinishReason;
  readonly usage?: TokenUsage;
  readonly error?: {
    readonly kind: ProviderErrorKind;
    readonly retryable: boolean;
    readonly message: string;
    /** Fail this many times before the step's success response is used. */
    readonly failTimes?: number;
  };
  readonly delayMs?: number;
};

export type FakeStep = {
  readonly match: {
    readonly purpose?: string | RegExp;
    readonly model?: string;
    /** Only match from the Nth call of the purpose onward (1-based). */
    readonly minCall?: number;
    /** Only match up to the Nth call of the purpose (1-based). */
    readonly maxCall?: number;
  };
  readonly response: FakeResponse;
};

export type FakeCallLogEntry = {
  readonly purpose: string | undefined;
  readonly model: string;
  readonly text: string;
};

/** Sleep that rejects promptly when the signal aborts. */
function sleep(ms: number, signal: AbortSignal | undefined): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(new DOMException('aborted', 'AbortError'));
      return;
    }
    const timer = setTimeout(() => {
      signal?.removeEventListener('abort', onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      reject(new DOMException('aborted', 'AbortError'));
    };
    signal?.addEventListener('abort', onAbort, { once: true });
  });
}

function lastUserText(request: ModelRequest): string {
  for (let i = request.messages.length - 1; i >= 0; i -= 1) {
    const message = request.messages[i];
    if (message !== undefined && message.role === 'user') return message.content;
  }
  return '';
}

export class FakeProvider implements ModelProvider {
  readonly name = 'fake' as const;
  readonly models: readonly string[] = ['fake-standard', 'fake-mini'];

  private readonly steps: readonly FakeStep[];
  private readonly fallbackResponse: FakeResponse | undefined;
  private readonly purposeCounters = new Map<string, number>();
  private readonly stepFailures = new Map<number, number>();
  private readonly log: FakeCallLogEntry[] = [];
  private callSeq = 0;

  constructor(steps: readonly FakeStep[] = [], fallbackResponse?: FakeResponse) {
    this.steps = steps;
    this.fallbackResponse = fallbackResponse;
  }

  /** Ordered log of every call this provider served. */
  get callLog(): readonly FakeCallLogEntry[] {
    return this.log;
  }

  /** Call counts keyed by purpose (undefined purposes share one key). */
  get callsByPurpose(): Readonly<Record<string, number>> {
    return Object.fromEntries(this.purposeCounters);
  }

  async *stream(request: ModelRequest): AsyncIterable<ProviderStreamEvent> {
    const purposeKey = request.purpose ?? '(none)';
    const count = (this.purposeCounters.get(purposeKey) ?? 0) + 1;
    this.purposeCounters.set(purposeKey, count);

    const response = this.matchStep(request, count);
    this.log.push({
      purpose: request.purpose,
      model: request.model,
      text: response.text ?? '',
    });

    try {
      if (response.delayMs !== undefined && response.delayMs > 0) {
        await sleep(response.delayMs, request.signal);
      }
    } catch {
      const reason = request.signal?.reason;
      const reasonText = typeof reason === 'string' && reason.length > 0 ? `: ${reason}` : '';
      yield {
        type: 'error',
        error: new ProviderFailure(
          'canceled',
          `fake provider call for purpose "${purposeKey}" was canceled during its delay${reasonText}`,
        ),
      };
      return;
    }

    if (response.error !== undefined) {
      yield {
        type: 'error',
        error: new ProviderFailure(response.error.kind, response.error.message, response.error.retryable),
      };
      return;
    }

    // Split text into a few deltas so consumers exercise delta folding.
    const text = response.text ?? '';
    if (text.length > 0) {
      const parts = splitText(text, 3);
      for (const part of parts) {
        yield { type: 'text-delta', text: part };
      }
    }

    this.callSeq += 1;
    for (const [index, call] of (response.toolCalls ?? []).entries()) {
      yield {
        type: 'tool-call',
        call: {
          id: `fake_${this.callSeq}_${index + 1}`,
          name: call.name,
          arguments: call.args,
        },
      };
    }

    if (response.structuredOutput !== undefined && request.structuredOutput !== undefined) {
      yield {
        type: 'structured-output',
        name: request.structuredOutput.name,
        value: response.structuredOutput,
      };
    }

    yield { type: 'usage', usage: response.usage ?? { inputTokens: 100, outputTokens: 50 } };

    const finishReason: FinishReason =
      response.finishReason ??
      ((response.toolCalls?.length ?? 0) > 0 ? 'tool-calls' : 'stop');
    yield {
      type: 'completed',
      finishReason,
      usage: response.usage ?? { inputTokens: 100, outputTokens: 50 },
    };
  }

  /**
   * Find the first step whose purpose/model/minCall constraints match, tracking
   * per-step failure counts so `error.failTimes` fails N times before the
   * step's success response takes over.
   */
  private matchStep(request: ModelRequest, count: number): FakeResponse {
    for (const [index, step] of this.steps.entries()) {
      const purposeMatched =
        step.match.purpose === undefined ||
        (request.purpose !== undefined &&
          (typeof step.match.purpose === 'string'
            ? step.match.purpose === request.purpose
            : step.match.purpose.test(request.purpose)));
      const modelMatched = step.match.model === undefined || step.match.model === request.model;
      const callMatched = (step.match.minCall ?? 0) <= count && count <= (step.match.maxCall ?? Number.MAX_SAFE_INTEGER);
      if (!purposeMatched || !modelMatched || !callMatched) continue;

      if (step.response.error !== undefined) {
        const failed = this.stepFailures.get(index) ?? 0;
        const failTimes = step.response.error.failTimes ?? 1;
        if (failed < failTimes) {
          this.stepFailures.set(index, failed + 1);
          return { error: step.response.error };
        }
        // Failures exhausted: serve the step's success payload.
        const { error: _exhausted, ...success } = step.response;
        return success;
      }
      return step.response;
    }
    if (this.fallbackResponse !== undefined) return this.fallbackResponse;
    return { text: lastUserText(request) };
  }
}

function splitText(text: string, parts: number): string[] {
  if (text.length <= 1) return [text];
  const size = Math.max(1, Math.ceil(text.length / parts));
  const chunks: string[] = [];
  for (let i = 0; i < text.length; i += size) {
    chunks.push(text.slice(i, i + size));
  }
  return chunks;
}

/** A fake provider that echoes the last user message. */
export function createEchoFakeProvider(): FakeProvider {
  return new FakeProvider([]);
}
