/**
 * Secret redaction and output bounding shared by every tool.
 *
 * Nothing a tool returns may carry credential material: known token shapes
 * are replaced with `[redacted:<kind>]` markers and the literal values of
 * secret-named environment variables never reach recorded output. The value
 * being redacted is deliberately never echoed in any message.
 */

import process from 'node:process';

/** Environment variable NAME patterns considered secret-bearing. */
export const SECRET_ENV_PATTERNS: readonly RegExp[] = [
  /(^|_)(API_?KEY|TOKENS?|SECRET|PASSWORD|PASSWD|PASSPHRASE|CREDENTIALS?|AUTH|AUTHORIZATION|BEARER)$/i,
  /(^|_)(PRIVATE_?KEY|ACCESS_?KEY|SECRET_?KEY|SIGNING_?KEY|SESSION_?KEY|CLIENT_?SECRET|REFRESH_?TOKEN)$/i,
  /(^|_)(DATABASE_?URL|DB_?URL|CONNECTION_?STRING|DSN)$/i,
];

/** True when an environment variable NAME looks secret-shaped. */
export function envNameLooksSecret(name: string): boolean {
  return SECRET_ENV_PATTERNS.some((pattern) => pattern.test(name));
}

type ValuePattern = {
  readonly pattern: RegExp;
  readonly kind: string;
};

/** Literal token shapes that must never appear in recorded output. */
const SECRET_VALUE_PATTERNS: readonly ValuePattern[] = [
  { pattern: /sk-ant-[A-Za-z0-9_-]{16,}/g, kind: 'anthropic-key' },
  { pattern: /sk-[A-Za-z0-9_-]{16,}/g, kind: 'api-key' },
  { pattern: /AKIA[0-9A-Z]{16}/g, kind: 'aws-access-key' },
  { pattern: /ghp_[A-Za-z0-9]{20,}/g, kind: 'github-token' },
  { pattern: /xox[bap]-[A-Za-z0-9-]{10,}/g, kind: 'slack-token' },
  { pattern: /\bBearer\s+[A-Za-z0-9._-]{20,}/g, kind: 'bearer-token' },
  {
    pattern: /-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|$)/g,
    kind: 'private-key',
  },
];

function replaceLiteral(text: string, value: string, marker: string): string {
  return text.split(value).join(marker);
}

/**
 * Redact known secret values from text: literal values of secret-named
 * environment variables (empty values are skipped) plus common token shapes.
 */
export function redactSecrets(text: string): string {
  let redacted = text;
  const envValues: readonly { value: string; marker: string }[] = Object.entries(process.env)
    .filter(([name, value]) => value !== undefined && value.length > 0 && envNameLooksSecret(name))
    .map(([, value]) => ({ value: value as string, marker: '[redacted:env]' }))
    .sort((a, b) => b.value.length - a.value.length);
  for (const entry of envValues) {
    redacted = replaceLiteral(redacted, entry.value, entry.marker);
  }
  for (const { pattern, kind } of SECRET_VALUE_PATTERNS) {
    redacted = redacted.replace(pattern, () => `[redacted:${kind}]`);
  }
  return redacted;
}

/** Cap text at `maxChars`, keeping the head and marking what was dropped. */
export function boundOutput(text: string, maxChars: number): string {
  if (maxChars <= 0) {
    throw new RangeError('boundOutput requires a positive maxChars');
  }
  if (text.length <= maxChars) {
    return text;
  }
  let keep = Math.max(0, maxChars - 1);
  let marker = `\n…[truncated ${text.length - keep} chars]`;
  while (keep > 0 && keep + marker.length > maxChars) {
    keep -= 1;
    marker = `\n…[truncated ${text.length - keep} chars]`;
  }
  if (keep + marker.length > maxChars) {
    return marker.slice(0, maxChars);
  }
  return text.slice(0, keep) + marker;
}

/**
 * Reduce environment variable entries to bare names for logging. Accepts
 * either plain names or `NAME=value` strings; values are never returned.
 */
export function sanitizeEnvForRecord(env: readonly string[]): string[] {
  const names = new Set<string>();
  for (const entry of env) {
    const equals = entry.indexOf('=');
    const name = (equals === -1 ? entry : entry.slice(0, equals)).trim();
    if (name.length > 0) {
      names.add(name);
    }
  }
  return [...names];
}
