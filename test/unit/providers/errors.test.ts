import { describe, expect, it } from 'vitest';
import { toProviderFailure } from '../../../src/providers/errors.js';
import { ProviderFailure } from '../../../src/providers/provider.js';

class SdkLikeError extends Error {
  constructor(
    readonly status: number | undefined,
    message: string,
    readonly headers?: { get: (name: string) => string | null },
  ) {
    super(message);
    this.name = 'APIError';
  }
}

function withCode(code: string): Error {
  const cause = new Error(`socket problem`);
  (cause as Error & { code?: string }).code = code;
  const wrapper = new Error('fetch failed', { cause });
  return wrapper;
}

describe('toProviderFailure', () => {
  it('passes ProviderFailure instances through untouched', () => {
    const failure = new ProviderFailure('auth', 'nope');
    expect(toProviderFailure(failure)).toBe(failure);
  });

  it('maps 401 and 403 to non-retryable auth failures', () => {
    for (const status of [401, 403]) {
      const failure = toProviderFailure(new SdkLikeError(status, 'bad key'));
      expect(failure.kind).toBe('auth');
      expect(failure.retryable).toBe(false);
      expect(failure.status).toBe(status);
      expect(failure.message).toBe('bad key');
    }
  });

  it('maps 408 to retryable timeout', () => {
    const failure = toProviderFailure(new SdkLikeError(408, 'request timeout'));
    expect(failure.kind).toBe('timeout');
    expect(failure.retryable).toBe(true);
  });

  it('maps 429 to retryable rate-limit and surfaces retry-after', () => {
    const error = new SdkLikeError(429, 'slow down', { get: () => '7' });
    const failure = toProviderFailure(error);
    expect(failure.kind).toBe('rate-limit');
    expect(failure.retryable).toBe(true);
    expect(failure.message).toContain('retry-after: 7s');
  });

  it('maps 5xx to retryable server failures', () => {
    for (const status of [500, 502, 503]) {
      const failure = toProviderFailure(new SdkLikeError(status, 'upstream exploded'));
      expect(failure.kind).toBe('server');
      expect(failure.retryable).toBe(true);
    }
  });

  it('maps connection resets in the cause chain to retryable network failures', () => {
    const failure = toProviderFailure(withCode('ECONNRESET'));
    expect(failure.kind).toBe('network');
    expect(failure.retryable).toBe(true);
    expect(failure.message).toBe('fetch failed');
  });

  it('maps ETIMEDOUT causes to timeout', () => {
    const failure = toProviderFailure(withCode('ETIMEDOUT'));
    expect(failure.kind).toBe('timeout');
    expect(failure.retryable).toBe(true);
  });

  it('maps abort errors to non-retryable cancellation', () => {
    const abort = new DOMException('This operation was aborted', 'AbortError');
    const failure = toProviderFailure(abort);
    expect(failure.kind).toBe('canceled');
    expect(failure.retryable).toBe(false);
  });

  it('maps SDK user-abort errors to cancellation', () => {
    const error = new Error('Request was aborted.');
    error.name = 'APIUserAbortError';
    const failure = toProviderFailure(error);
    expect(failure.kind).toBe('canceled');
  });

  it('maps other 4xx to non-retryable unknown failures', () => {
    const failure = toProviderFailure(new SdkLikeError(422, 'unprocessable'));
    expect(failure.kind).toBe('unknown');
    expect(failure.retryable).toBe(false);
    expect(failure.status).toBe(422);
  });

  it('falls back to unknown for plain errors', () => {
    const failure = toProviderFailure(new Error('weird'));
    expect(failure.kind).toBe('unknown');
    expect(failure.retryable).toBe(false);
    expect(failure.message).toBe('weird');
  });

  it('trims messages to 300 characters and never includes response bodies', () => {
    const long = 'x'.repeat(500);
    const failure = toProviderFailure(new SdkLikeError(500, long));
    expect(failure.message.length).toBe(300);
  });

  it('redacts credential-shaped substrings from messages', () => {
    const failure = toProviderFailure(new Error('key sk-abc123def456ghi789 rejected'));
    expect(failure.message).not.toContain('sk-abc123def456ghi789');
    expect(failure.message).toContain('[redacted]');
  });

  it('redacts anthropic keys and bearer tokens', () => {
    const a = toProviderFailure(new Error('leak sk-ant-abc123def456 rejected'));
    expect(a.message).toContain('[redacted]');
    const b = toProviderFailure(new Error('header Bearer eyJhbGciOi.abc123 invalid'));
    expect(b.message).toContain('Bearer [redacted]');
  });
});
