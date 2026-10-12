/**
 * Normalize provider SDK errors into Relay-owned failures.
 *
 * SDK errors (OpenAI `APIError`, Anthropic `APIError`, fetch/network errors)
 * carry request headers and bodies that must never reach Relay's logs. This
 * mapper keeps only a trimmed `error.message`, classifies the failure into a
 * provider-neutral kind with a retryability hint, and never serializes request
 * or response payloads.
 */

import { ProviderFailure, type ProviderErrorKind } from './provider.js';

/** Maximum length of a preserved error message. */
const MAX_MESSAGE_CHARS = 300;

const NETWORK_CODES = new Set([
  'ECONNRESET',
  'ECONNREFUSED',
  'EPIPE',
  'ENOTFOUND',
  'EAI_AGAIN',
  'ECONNABORTED',
  'EHOSTUNREACH',
  'ENETUNREACH',
  'EPROTO',
]);

const TIMEOUT_CODES = new Set(['ETIMEDOUT', 'ETIMEOUT']);

type ErrorLike = {
  message?: unknown;
  name?: unknown;
  code?: unknown;
  status?: unknown;
  cause?: unknown;
  headers?: { get?: (name: string) => string | null | undefined };
};

function asErrorLike(error: unknown): ErrorLike | null {
  if (typeof error === 'string') return { message: error };
  if (typeof error !== 'object' || error === null) return null;
  return error as ErrorLike;
}

function isAbortLike(error: ErrorLike): boolean {
  if (error.name === 'AbortError' || error.name === 'APIUserAbortError') return true;
  if (error.code === 'ABORT_ERR' || error.code === 20) return true;
  if (error.message === 'This operation was aborted') return true;
  return false;
}

/** Walk the `cause` chain (bounded) looking for classification signals. */
function findCauseSignal(error: ErrorLike, depth = 0): ErrorLike | null {
  let current: ErrorLike | undefined = error;
  for (let i = 0; i < depth && current; i += 1) {
    const cause = asErrorLike(current.cause);
    current = cause ?? undefined;
  }
  return current ?? null;
}

/** Redact credential-shaped substrings so keys never reach persisted output. */
function redactMessage(text: string): string {
  return text
    .replace(/sk-ant-[A-Za-z0-9_-]{8,}/g, '[redacted]')
    .replace(/sk-[A-Za-z0-9_-]{8,}/g, '[redacted]')
    .replace(/\bBearer\s+[A-Za-z0-9._~+/=-]{8,}/gi, 'Bearer [redacted]');
}

function safeMessage(error: ErrorLike, fallback: string): string {
  const raw = typeof error.message === 'string' && error.message.length > 0 ? error.message : fallback;
  return redactMessage(raw).trim().slice(0, MAX_MESSAGE_CHARS);
}

function readRetryAfter(error: ErrorLike): string | null {
  const get = error.headers?.get?.bind(error.headers);
  if (typeof get !== 'function') return null;
  try {
    const value = get('retry-after');
    return typeof value === 'string' && value.length > 0 ? value : null;
  } catch {
    return null;
  }
}

/**
 * Map an unknown thrown value to a `ProviderFailure`. Classification order:
 * aborts, then HTTP status codes, then connection-level causes, then SDK error
 * class names, and finally `unknown`.
 */
export function toProviderFailure(error: unknown): ProviderFailure {
  if (error instanceof ProviderFailure) return error;

  const direct = asErrorLike(error);
  if (!direct) {
    return new ProviderFailure('unknown', redactMessage(String(error)).slice(0, MAX_MESSAGE_CHARS));
  }

  if (isAbortLike(direct)) {
    return new ProviderFailure('canceled', safeMessage(direct, 'request aborted'));
  }

  const status = typeof direct.status === 'number' ? direct.status : undefined;

  if (status === 401 || status === 403) {
    return new ProviderFailure(
      'auth',
      safeMessage(direct, `provider rejected credentials (HTTP ${status})`),
      false,
      status,
    );
  }
  if (status === 408) {
    return new ProviderFailure('timeout', safeMessage(direct, 'provider timed out (HTTP 408)'), true, status);
  }
  if (status === 429) {
    const retryAfter = readRetryAfter(direct);
    const suffix = retryAfter ? ` (retry-after: ${retryAfter}s)` : '';
    return new ProviderFailure(
      'rate-limit',
      safeMessage(direct, 'provider rate limited the request') + suffix,
      true,
      status,
    );
  }
  if (status !== undefined && status >= 500) {
    return new ProviderFailure('server', safeMessage(direct, `provider server error (HTTP ${status})`), true, status);
  }

  // Connection-level failures are usually wrapped in a `cause` (fetch throws
  // TypeError('fetch failed') with the socket error underneath).
  for (const depth of [1, 2, 3]) {
    const cause = findCauseSignal(direct, depth);
    if (!cause) break;
    if (isAbortLike(cause)) {
      return new ProviderFailure('canceled', safeMessage(direct, 'request aborted'));
    }
    const code = typeof cause.code === 'string' ? cause.code : undefined;
    if (code && NETWORK_CODES.has(code)) {
      return new ProviderFailure('network', safeMessage(direct, `network error: ${code}`), true);
    }
    if (code && TIMEOUT_CODES.has(code)) {
      return new ProviderFailure('timeout', safeMessage(direct, `network timeout: ${code}`), true);
    }
    if (cause.name === 'APIConnectionTimeoutError') {
      return new ProviderFailure('timeout', safeMessage(direct, 'connection timed out'), true);
    }
  }

  if (direct.name === 'APIConnectionTimeoutError') {
    return new ProviderFailure('timeout', safeMessage(direct, 'connection timed out'), true);
  }
  if (direct.name === 'APIConnectionError') {
    return new ProviderFailure('network', safeMessage(direct, 'connection failed'), true);
  }

  const kind: ProviderErrorKind = 'unknown';
  const fallback =
    status !== undefined ? `provider error (HTTP ${status})` : 'unclassified provider error';
  return new ProviderFailure(kind, safeMessage(direct, fallback), false, status);
}
