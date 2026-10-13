/**
 * Canonical JSON serialization and hash-chain helpers.
 *
 * This module is the TypeScript half of the cross-language canonicalization
 * contract in `docs/event-format.md` §2–§3. The Rust `relay-core` crate
 * implements the same byte-for-byte rules and is held to them by the fixtures
 * generated with this module:
 *
 * 1. UTF-8 throughout; no BOM; no insignificant whitespace.
 * 2. Object keys sorted lexicographically by Unicode code point (equivalently,
 *    byte-wise over UTF-8 encoded keys), recursively.
 * 3. Strings escaped minimally: `"` as `\"`, backslash as `\\`, and control
 *    characters as `\b`, `\f`, `\n`, `\r`, `\t` or `\u00xx` (lowercase hex)
 *    for the remaining C0 range. Non-ASCII characters are emitted as raw
 *    UTF-8, never `\u`-escaped.
 * 4. Integers serialize as exact decimal integers. Non-integer numbers (only
 *    `confidence` values bounded to [0, 1] exist in Relay events) serialize as
 *    the shortest decimal representation that round-trips to the same IEEE-754
 *    binary64 value; a confidence of exactly 0 or 1 is written as `0`/`1`.
 * 5. `true`, `false`, `null` serialize as the JSON literals.
 *
 * Pure functions only — no IO, no clocks, no randomness.
 */

import { createHash } from 'node:crypto';

/** Compare two strings by their UTF-8 bytes (canonical key order). */
function compareUtf8(a, b) {
  return Buffer.compare(Buffer.from(a, 'utf8'), Buffer.from(b, 'utf8'));
}

/** Escape a string per §2 rule 3. */
export function canonicalString(value) {
  let out = '"';
  for (const ch of value) {
    const code = ch.codePointAt(0);
    if (ch === '"') {
      out += '\\"';
    } else if (ch === '\\') {
      out += '\\\\';
    } else if (code === 0x08) {
      out += '\\b';
    } else if (code === 0x09) {
      out += '\\t';
    } else if (code === 0x0a) {
      out += '\\n';
    } else if (code === 0x0c) {
      out += '\\f';
    } else if (code === 0x0d) {
      out += '\\r';
    } else if (code < 0x20) {
      out += `\\u${code.toString(16).padStart(4, '0')}`;
    } else {
      out += ch;
    }
  }
  return `${out}"`;
}

/** Serialize a number per §2 rules 4–5 (JS `Number.prototype.toString` semantics). */
export function canonicalNumber(value) {
  if (!Number.isFinite(value)) {
    throw new TypeError(`canonical JSON cannot serialize non-finite number: ${String(value)}`);
  }
  if (Number.isInteger(value)) {
    // Covers the integral case including exact 0/1 confidences; JS never
    // emits a ".0" suffix or a "+0" sign.
    return String(value);
  }
  return String(value);
}

/** Recursively canonicalize a parsed JSON value. `undefined` object values are skipped. */
export function canonicalize(value) {
  if (value === null) return 'null';
  switch (typeof value) {
    case 'boolean':
      return value ? 'true' : 'false';
    case 'number':
      return canonicalNumber(value);
    case 'string':
      return canonicalString(value);
    case 'object':
      break;
    default:
      throw new TypeError(`canonical JSON cannot serialize ${typeof value}`);
  }
  if (Array.isArray(value)) {
    const parts = value.map((entry) => canonicalize(entry));
    return `[${parts.join(',')}]`;
  }
  const keys = Object.keys(value)
    .filter((key) => value[key] !== undefined)
    .sort(compareUtf8);
  const parts = keys.map((key) => `${canonicalString(key)}:${canonicalize(value[key])}`);
  return `{${parts.join(',')}}`;
}

/** Lowercase hex SHA-256 over the UTF-8 encoding of `text`. */
export function sha256Hex(text) {
  return createHash('sha256').update(text, 'utf8').digest('hex');
}

/** SHA-256 over the canonical serialization of `envelope` minus its `hash` field. */
export function computeEnvelopeHash(envelope) {
  const { hash: _hash, ...withoutHash } = envelope;
  return sha256Hex(canonicalize(withoutHash));
}

/**
 * Attach envelopeVersion/previousHash/hash chain metadata to already-sequenced
 * envelopes, in log order. Returns new objects; the input is not mutated.
 */
export function withHashChain(envelopes, envelopeVersion = 1) {
  let previousHash = null;
  return envelopes.map((envelope) => {
    const chained = { ...envelope, envelopeVersion, previousHash };
    const hash = computeEnvelopeHash(chained);
    previousHash = hash;
    return { ...chained, hash };
  });
}

/** Render one chained envelope as a canonical JSONL line (no trailing newline). */
export function toCanonicalLine(envelope) {
  return canonicalize(envelope);
}

/**
 * Verify every line of a canonical JSONL event log. Returns a list of
 * violations; empty means the log is canonically serialized and correctly
 * hash-chained. `options.expectedEnvelopeVersion` overrides the envelope
 * version every line must carry (used when generating deliberately-newer
 * envelope fixtures).
 */
export function verifyCanonicalLog(text, options) {
  const expectedEnvelopeVersion = options?.expectedEnvelopeVersion ?? 1;
  const violations = [];
  const lines = text.split('\n');
  if (lines[lines.length - 1] !== '') {
    violations.push('log does not end with a newline');
  }
  const body = lines[lines.length - 1] === '' ? lines.slice(0, -1) : lines;
  let expectedPreviousHash = null;
  body.forEach((line, index) => {
    const where = `line ${index + 1}`;
    if (line.length === 0) {
      violations.push(`${where}: blank line`);
      return;
    }
    let parsed;
    try {
      parsed = JSON.parse(line);
    } catch (error) {
      violations.push(`${where}: not valid JSON (${error instanceof Error ? error.message : String(error)})`);
      return;
    }
    if (canonicalize(parsed) !== line) {
      violations.push(`${where}: not canonically serialized`);
    }
    if (parsed === null || typeof parsed !== 'object') {
      violations.push(`${where}: not a JSON object`);
      return;
    }
    if (parsed.envelopeVersion !== expectedEnvelopeVersion) {
      violations.push(
        `${where}: envelopeVersion ${String(parsed.envelopeVersion)} !== ${expectedEnvelopeVersion}`,
      );
    }
    const expectedHash = computeEnvelopeHash(parsed);
    if (parsed.hash !== expectedHash) {
      violations.push(`${where}: hash mismatch (expected ${expectedHash}, found ${String(parsed.hash)})`);
    }
    if (parsed.previousHash !== expectedPreviousHash) {
      violations.push(
        `${where}: previousHash mismatch (expected ${String(expectedPreviousHash)}, found ${String(parsed.previousHash)})`,
      );
    }
    expectedPreviousHash = expectedHash;
  });
  return violations;
}
