/**
 * Type declarations for the canonical JSON helper module.
 *
 * These exist so TypeScript consumers under `test/` can import the .mjs
 * module (the implementation is plain JavaScript shared with the exporter
 * scripts).
 */

/** A parsed JSON value: what the canonicalizer accepts. */
export type CanonicalValue =
  | null
  | boolean
  | number
  | string
  | CanonicalValue[]
  | { [key: string]: CanonicalValue | undefined };

/** Escape a string per docs/event-format.md §2 rule 3. */
export function canonicalString(value: string): string;

/** Serialize a number per §2 rules 4–5 (JS `Number.prototype.toString` semantics). */
export function canonicalNumber(value: number): string;

/**
 * Recursively canonicalize a parsed JSON value (`undefined` object values are
 * skipped). Accepts `unknown` because callers pass domain view objects that
 * are structurally JSON; the runtime throws for non-JSON values.
 */
export function canonicalize(value: unknown): string;

/** Lowercase hex SHA-256 over the UTF-8 encoding of `text`. */
export function sha256Hex(text: string): string;

/** SHA-256 over the canonical serialization of `envelope` minus its `hash` field. */
export function computeEnvelopeHash(envelope: Record<string, unknown>): string;

/**
 * Attach envelopeVersion/previousHash/hash chain metadata to already-sequenced
 * envelopes, in log order.
 */
export function withHashChain(
  envelopes: ReadonlyArray<Record<string, unknown>>,
  envelopeVersion?: number,
): Record<string, unknown>[];

/** Render one chained envelope as a canonical JSONL line (no trailing newline). */
export function toCanonicalLine(envelope: Record<string, unknown>): string;

/**
 * Verify every line of a canonical JSONL event log; returns violations
 * (empty when the log is canonically serialized and correctly hash-chained).
 */
export function verifyCanonicalLog(
  text: string,
  options?: { readonly expectedEnvelopeVersion?: number },
): string[];
