import { describe, expect, it } from 'vitest';
import { ProviderFailure } from '../../../src/providers/provider.js';
import {
  MAX_ATTEMPTS,
  backoffDelayMs,
  isRetryable,
  withRetries,
} from '../../../src/runtime/retry.js';

function failure(kind: ConstructorParameters<typeof ProviderFailure>[0], retryable: boolean): ProviderFailure {
  return new ProviderFailure(kind, 'msg', retryable);
}

describe('retry', () => {
  it('exports the default attempt cap', () => {
    expect(MAX_ATTEMPTS).toBe(3);
  });

  it('isRetryable reflects the retryable flag and never cancels', () => {
    expect(isRetryable(failure('server', true))).toBe(true);
    expect(isRetryable(failure('rate-limit', true))).toBe(true);
    expect(isRetryable(failure('network', true))).toBe(true);
    expect(isRetryable(failure('timeout', true))).toBe(true);
    expect(isRetryable(failure('auth', false))).toBe(false);
    expect(isRetryable(failure('invalid-response', false))).toBe(false);
    expect(isRetryable(failure('canceled', true))).toBe(false);
  });

  it('backoffDelayMs grows exponentially with injectable jitter', () => {
    expect(backoffDelayMs(1, () => 0)).toBe(250);
    expect(backoffDelayMs(2, () => 0)).toBe(500);
    expect(backoffDelayMs(3, () => 0)).toBe(1000);
    expect(backoffDelayMs(4, () => 0)).toBe(2000);
    expect(backoffDelayMs(2, () => 0.5)).toBe(550); // 500 + 50 jitter
    expect(backoffDelayMs(1, () => 0.999)).toBeLessThan(350);
  });

  it('retries retryable failures until success and passes attempt numbers', async () => {
    const sleeps: number[] = [];
    const attempts: number[] = [];
    let calls = 0;
    const result = await withRetries(
      async (attempt) => {
        attempts.push(attempt);
        calls += 1;
        if (calls < 3) throw failure('server', true);
        return 'ok';
      },
      { sleep: async (ms) => {
        sleeps.push(ms);
      } },
    );
    expect(result).toBe('ok');
    expect(attempts).toEqual([1, 2, 3]);
    expect(sleeps.length).toBe(2);
  });

  it('gives up after maxAttempts and rethrows the last failure', async () => {
    let calls = 0;
    await expect(
      withRetries(
        async () => {
          calls += 1;
          throw failure('rate-limit', true, );
        },
        { sleep: async () => undefined },
      ),
    ).rejects.toMatchObject({ kind: 'rate-limit' });
    expect(calls).toBe(MAX_ATTEMPTS);
  });

  it('honors a smaller maxAttempts option', async () => {
    let calls = 0;
    await expect(
      withRetries(
        async () => {
          calls += 1;
          throw failure('network', true);
        },
        { maxAttempts: 2, sleep: async () => undefined },
      ),
    ).rejects.toMatchObject({ kind: 'network' });
    expect(calls).toBe(2);
  });

  it('does not retry non-retryable failures', async () => {
    let calls = 0;
    await expect(
      withRetries(
        async () => {
          calls += 1;
          throw failure('auth', false);
        },
        { sleep: async () => undefined },
      ),
    ).rejects.toMatchObject({ kind: 'auth' });
    expect(calls).toBe(1);
  });

  it('does not retry canceled failures', async () => {
    let calls = 0;
    await expect(
      withRetries(
        async () => {
          calls += 1;
          throw failure('canceled', true);
        },
        { sleep: async () => undefined },
      ),
    ).rejects.toMatchObject({ kind: 'canceled' });
    expect(calls).toBe(1);
  });

  it('rethrows non-ProviderFailure errors untouched', async () => {
    const error = new Error('programmer error');
    await expect(
      withRetries(async () => {
        throw error;
      }),
    ).rejects.toBe(error);
  });

  it('unwraps ProviderFailures carried as a cause', async () => {
    let calls = 0;
    await expect(
      withRetries(
        async () => {
          calls += 1;
          throw new Error('wrapped', { cause: failure('server', true) });
        },
        { maxAttempts: 1, sleep: async () => undefined },
      ),
    ).rejects.toMatchObject({ kind: 'server' });
    expect(calls).toBe(1);
  });

  it('stops retrying when the signal aborts during backoff', async () => {
    const controller = new AbortController();
    let calls = 0;
    const promise = withRetries(
      async () => {
        calls += 1;
        throw failure('server', true);
      },
      {
        sleep: async () => {
          controller.abort('user');
        },
        signal: controller.signal,
      },
    );
    await expect(promise).rejects.toMatchObject({ kind: 'canceled' });
    expect(calls).toBe(1);
  });
});
