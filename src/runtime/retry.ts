/**
 * Bounded retry for transient provider failures.
 *
 * Only provider failures marked retryable (rate limits, 5xx, network blips,
 * timeouts) are retried, with exponential backoff plus jitter. Cancellation is
 * never retried and non-retryable failures (auth, invalid responses) fail
 * fast so the agent loop can move to a fallback route.
 */

import { ProviderFailure } from '../providers/provider.js';

export const MAX_ATTEMPTS = 3;

const BASE_DELAY_MS = 250;
const JITTER_CEILING = 100;

/** Whether a provider failure is transient enough to retry. */
export function isRetryable(failure: ProviderFailure): boolean {
  return failure.retryable && failure.kind !== 'canceled';
}

/**
 * Backoff for a 1-based attempt number: 250ms * 2^(attempt-1) plus jitter.
 * The jitter source is injectable so tests get deterministic delays.
 */
export function backoffDelayMs(attempt: number, jitter: () => number = Math.random): number {
  const exp = BASE_DELAY_MS * 2 ** (Math.max(1, attempt) - 1);
  return exp + Math.floor(jitter() * JITTER_CEILING);
}

export type RetryOptions = {
  readonly maxAttempts?: number;
  /** Injectable sleep for tests; defaults to a real timer. */
  readonly sleep?: (ms: number) => Promise<void>;
  /** Aborting this signal stops further retries. */
  readonly signal?: AbortSignal;
};

const defaultSleep = (ms: number): Promise<void> =>
  new Promise((resolve) => {
    setTimeout(resolve, ms);
  });

/** Unwrap a ProviderFailure from itself or an error carrying one as its cause. */
function asProviderFailure(error: unknown): ProviderFailure | null {
  if (error instanceof ProviderFailure) return error;
  if (error instanceof Error && error.cause instanceof ProviderFailure) return error.cause;
  return null;
}

/**
 * Run `fn` with bounded retries. `fn` receives the 1-based attempt number.
 * Throws the last failure when attempts are exhausted, when the failure is not
 * retryable, or when the signal aborts during backoff (as a canceled
 * ProviderFailure).
 */
export async function withRetries<T>(
  fn: (attempt: number) => Promise<T>,
  options: RetryOptions = {},
): Promise<T> {
  const maxAttempts = options.maxAttempts ?? MAX_ATTEMPTS;
  const sleep = options.sleep ?? defaultSleep;

  let attempt = 0;
  // eslint-disable-next-line no-constant-condition
  while (true) {
    attempt += 1;
    try {
      return await fn(attempt);
    } catch (error) {
      const failure = asProviderFailure(error);
      if (failure === null) throw error;
      if (failure.kind === 'canceled') throw failure;
      if (!isRetryable(failure)) throw failure;
      if (attempt >= maxAttempts) throw failure;
      await sleep(backoffDelayMs(attempt));
      if (options.signal?.aborted) {
        throw new ProviderFailure('canceled', 'canceled while waiting to retry a provider failure');
      }
    }
  }
}
