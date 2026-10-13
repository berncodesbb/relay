# Relay canonical event format

Status: LEGACY — normative only for the stopped compatibility-port event
path (`relay-core` events + the legacy `RelayStore` surface), which
remains in the repository as frozen reference until its documented
retirement gate. The intent-first work-graph store is specified by
`docs/plans/fact-format-proposal.md` (accepted 2026-10-13); new
persistence work targets that format. This document changes nothing and
is retained as the reference for the legacy importer and old fixtures.
Date: 2026-10-13
Owners: lead integrator (spec), storage/migration owner (implementation), core/compatibility owner (payload schemas)

This document specifies the authoritative Relay event record: the JSONL
envelope, canonical JSON serialization, the hash chain, segmentation, the
store manifest, durability, recovery, and the rules for comparing output
across the TypeScript and Rust implementations.

The authority order is fixed:

```text
canonical JSONL events -> deterministic replay -> current project state
                       -> SQLite projections -> efficient queries
                       -> Markdown artifacts -> human review
```

If JSONL and SQLite disagree, JSONL wins. SQLite is disposable and
rebuildable. Markdown artifacts are immutable human-readable records.

## 1. Event envelope

Each durable workflow change is one JSON object on one physical line in a
segmented JSONL log. The Rust envelope extends the logical fields Relay
already persists with chain metadata:

```json
{
  "envelopeVersion": 1,
  "id": "evt_01234567-89ab-cdef-0123-456789abcdef",
  "projectId": "prj_01234567-89ab-cdef-0123-456789abcdef",
  "sequence": 42,
  "sessionId": "ses_..." ,
  "type": "artifact.approved",
  "schemaVersion": 1,
  "timestamp": "2026-10-13T08:00:00.123Z",
  "correlationId": "cor_...",
  "causationId": null,
  "payload": { },
  "previousHash": "3b5d…hex…",
  "hash": "9f1c…hex…"
}
```

Field rules:

| Field | Type | Rules |
| --- | --- | --- |
| `envelopeVersion` | integer | Always `1` for this format. Readers reject newer envelope versions fail-closed with upgrade guidance. |
| `id` | string | `<prefix>_<uuid-v4-lowercase>`. Prefixes in use: `evt`, `cor`, `ses`, `prj`, `art`, `run`. Unique within the log. (Historical note: the TypeScript runtime emits `session_`-prefixed session ids in some paths; readers accept both `ses_` and `session_`.) |
| `projectId` | string | `prj_<uuid>`. Every event in one log belongs to exactly one project. |
| `sequence` | integer | 1-based, contiguous, increasing by exactly 1 across the whole project log (not per segment). |
| `sessionId` | string \| null | Session that produced the event, or `null`. |
| `type` | string | One of the 24 event types in `src/core/events.ts` (`RELAY_EVENT_TYPES`). Unknown types fail replay fail-closed. |
| `schemaVersion` | integer | Per-type payload version; must equal the version the reader understands (all are `1` today). |
| `timestamp` | string | RFC 3339 UTC with millisecond precision and a `Z` suffix: `YYYY-MM-DDTHH:MM:SS.sssZ` (matches JS `Date.toISOString()`). |
| `correlationId` | string | Fresh `cor_<uuid>` when the event starts an operation; the initiator's correlation id when it is part of one. |
| `causationId` | string \| null | `id` of the event that directly caused this one, or `null`. |
| `payload` | object | Type-specific payload, byte-compatible with the TypeScript Zod schemas. Keys and values use exactly the TypeScript field names. |
| `previousHash` | string \| null | Hex SHA-256 of the previous event's `hash` input; `null` exactly when `sequence === 1`. |
| `hash` | string | Hex SHA-256 (lowercase) over the canonical serialization of this envelope minus the `hash` field itself. |

Additive envelope or payload fields are permitted for forward compatibility:
readers must accept objects containing unknown additional fields at the
envelope level only if `envelopeVersion` is understood. Unknown payload
fields are ignored (stripped on decode), matching the TypeScript oracle's
Zod default behavior — the schema constrains the fields it knows, and
unknown extras never invalidate a legacy record. Verified decision from
cross-language fixture work; earlier drafts of this section said payload
decoding was strict, which the TypeScript implementation does not do.

## 2. Canonical JSON

Canonical serialization is used for (a) the bytes written into JSONL
segments, (b) the hash input, and (c) the payload strings the TypeScript
exporter emits. Readers must re-serialize after parsing; they must never
hash raw input bytes.

Rules:

1. UTF-8 throughout; no BOM; no insignificant whitespace; no line breaks
   outside JSON string escapes.
2. Object keys are sorted lexicographically by Unicode code point (equival:
   byte-wise over UTF-8 encoded keys), recursively.
3. Strings are escaped minimally: `"` as `\"`, backslash as `\\`, and
   control characters as `\b`, `\f`, `\n`, `\r`, `\t` or `\u00xx`
   (lowercase hex) for the remaining C0 range. Non-ASCII characters are
   emitted as raw UTF-8, never `\u`-escaped.
4. Integers serialize as exact decimal integers, no exponent, no `.0`
   suffix, no leading zeros or `+` sign.
5. Only two classes of non-integer numbers exist in Relay events:
   `intent.applied.confidence` and `intent.proposed.proposal.confidence`,
   both bounded `[0, 1]`. They serialize as the shortest decimal
   representation that round-trips to the same IEEE-754 binary64 value
   (Rust Ryu and JS `Number.prototype.toString` agree on this range).
   A confidence of exactly `0` or `1` must be written as `0`/`1`.
6. `true`, `false`, `null` serialize as the JSON literals.
7. Each physical line in a segment is one canonical event object followed
   by `\n` (LF, `0x0A`).

## 3. Hash chain

`hash = sha256(canonical(envelope_without_hash))` — the envelope object with
`envelopeVersion`, `id`, `projectId`, `sequence`, `sessionId`, `type`,
`schemaVersion`, `timestamp`, `correlationId`, `causationId`, `payload`,
`previousHash`, all keys canonically sorted, UTF-8 encoded.

`previousHash` equals the `hash` of the event at `sequence - 1` and is
`null` at sequence 1. The chain is per project. Verification recomputes
every hash; a mismatch fails with the segment name, line number, sequence,
and the expected/actual digests. The chain detects truncation, reordering,
and accidental edits; it is not an authentication mechanism against a
malicious local user, and never involves secrets.

## 4. Segmentation

```text
.relay/
  manifest.json
  events/
    0000000000000001-0000000000000500.jsonl
    0000000000000501-open.jsonl
  index.sqlite
  artifacts/
    prd/v2-<slug>.md
  locks/
    writer.lock
  recovery/
    rejected-tail-20261013T080000Z.jsonl
```

- Segment file names carry 16-digit zero-padded hexadecimal numbers.
- A closed segment is named `<firstSequence>-<lastSequence>.jsonl` and is
  immutable. Only the final `…-open.jsonl` segment may grow.
- The active segment is named `<firstSequence>-open.jsonl`.
- Rotation happens after a successful durable append when the active
  segment holds ≥ 1000 events or ≥ 8 MiB, whichever comes first: the writer
  renames it to its closed name and creates a fresh active segment.
- Segment discovery is deterministic: `events/` entries matching the name
  patterns, sorted lexicographically; closed segments must tile the
  sequence space contiguously ending exactly one below the active segment's
  first sequence. Unknown files in `events/` are reported by `doctor`
  but do not block opening.
- Closed segments must end with `\n` and contain only complete valid
  records. The active segment may end mid-line only as a crash artifact
  (see §7).

## 5. Store manifest

`.relay/manifest.json` holds store metadata only — never workflow state:

```json
{
  "storeVersion": 1,
  "projectId": "prj_…",
  "createdAt": "2026-10-13T08:00:00.000Z",
  "activeSegment": "0000000000000501-open.jsonl",
  "minReaderVersion": "0.2.0"
}
```

`storeVersion` is the JSONL/container format version (currently `1`).
Readers that see a newer `storeVersion` fail closed with an upgrade
instruction. The manifest is rewritten atomically (write temp + rename)
whenever the active segment changes.

## 6. Append protocol and durability

Exactly one process may append at a time, enforced by the writer lock
(§8). The protocol:

1. Acquire `.relay/locks/writer.lock` with a bounded timeout (default
   5000 ms, matching the legacy SQLite busy timeout).
2. Read and validate the manifest and the active segment tail.
3. Verify the expected next sequence and previous hash.
4. Validate the entire proposed batch against the domain payload schemas.
5. Serialize the batch into one buffer, one canonical event per line.
6. Append the whole buffer to the active segment with a single write.
7. Flush and fsync the segment file in `durable` mode (the only mode used
   for contractual state in v1; a `relaxed` test mode skips fsync).
   Best-effort fsync of the containing directory after file creation or
   rename.
8. Commit the corresponding SQLite projection updates in one transaction
   and advance the projection checkpoint.
9. Release the lock.

The user-visible success boundary is the durable JSONL append (step 7). If
the process dies between steps 7 and 8, the next open detects that SQLite
is behind and replays the missing suffix. SQLite must never advance beyond
a validated JSONL event, and no operation may report success before the
event is durable. Segment rotation (§4) happens after step 7, before step
10's lock release.

The lock is held only for storage operations — never across provider
requests or tool execution.

## 7. Tail recovery and corruption policy

On open, all segments are validated. Rules:

- Any complete-but-invalid record (unknown type, newer schema version,
  failed payload validation, hash mismatch, sequence gap, duplicate id,
  project mismatch) fails the open with the segment name, 1-based line
  number, sequence, and reason. Complete invalid history is never
  silently removed.
- The single allowed automatic repair: an incomplete final byte tail of
  the active segment (file does not end with `\n`). While holding the
  writer lock, the incomplete bytes are copied to
  `recovery/rejected-tail-<UTC-timestamp>.jsonl`, then the active file is
  truncated to the last valid newline. The repair is reported to the user
  with the recovery path.
- A closed segment with an incomplete final line, or an incomplete tail
  that is followed by more data, fails the open (not a crash tail).
- `doctor check` re-validates everything read-only and reports findings;
  `doctor rebuild-index` rebuilds SQLite exclusively from JSONL.

## 8. Writer lock

`.relay/locks/writer.lock` is created exclusively (`O_CREAT | O_EXCL`, or
`CreateNew` on Windows) and contains canonical JSON:

```json
{ "pid": 12345, "acquiredAt": "2026-10-13T08:00:00.000Z" }
```

- Acquisition retries with a short sleep until the bounded timeout, then
  fails with a readable diagnostic that includes the holder pid and age.
- A lock whose owning process no longer exists (PID liveness probe:
  `kill(pid, 0)` on Unix, process-open on Windows) is treated as stale,
  moved to `recovery/`, and re-acquired.
- The lock is removed on normal release. Best-effort only: the stale probe
  is the real guard against crashed holders.

## 9. SQLite projections

`index.sqlite` is derived state. Its schema version is independent of the
JSONL store version. Required contents:

- `events`: id, project, sequence, session, type, schema version,
  timestamp, correlation, causation, payload (canonical JSON string),
  hash — indexed for lookup by id/sequence/type/session/correlation/time.
- `projects_summary`: current mode, autonomy, goal digest fields, paused
  flag, artifact counts, verification summary.
- `sessions`: session ids, start times, interactive flag, event counts.
- `artifacts`: artifact id, kind, version, status, path.
- `model_calls`: provider, model, role, purpose, usage, failure summary.
- `tool_calls`: tool, capability, outcome, duration, denial reasons.
- `checkpoint`: one row — last indexed sequence and hash.

Reconciliation on open:

- checkpoint equals JSONL tail → open normally;
- checkpoint behind → replay the missing suffix into SQLite;
- database missing, unreadable, integrity check failed, projection schema
  newer, or checkpoint ahead of / divergent from JSONL → rename the file
  into `recovery/` and rebuild entirely from JSONL.

Rebuild never touches JSONL or artifacts. Deleting `index.sqlite` between
runs must always produce an equivalent database.

## 10. Legacy import (`.relay/state.db` → canonical store)

The legacy TypeScript store keeps events in a SQLite table `events` with
columns `id, project_id, sequence, session_id, type, schema_version,
timestamp, correlation_id, causation_id, payload` (payload a JSON string)
plus derived tables. Import is direct and read-only against the source:

1. Acquire a migration lock; refuse if a Relay writer is active.
2. `PRAGMA integrity_check` on the source; any failure aborts.
3. Read every row ordered by `project_id, sequence`; validate each with
   the domain schemas (same errors as replay would give).
4. Verify contiguous sequences from 1 and that legacy replay succeeds.
5. Write the new store into a staging sibling
   `.relay-rust-migration-<projectId>/` (manifest, segments, hashes —
   hashes are computed fresh because legacy rows carry none).
6. Copy artifact files byte-for-byte into the staging store, recording
   SHA-256 content digests.
7. Replay the staged JSONL and compare the normalized final state with a
   fresh legacy replay; mismatch aborts the migration.
8. Build SQLite projections from JSONL in the staging store.
9. Atomically rename the old `.relay/` to `.relay-backup-<UTC-stamp>/`
   and rename the staging directory to `.relay/`. The backup is retained
   indefinitely; nothing deletes it automatically.
10. Any failure before step 9 leaves the original `.relay/` untouched and
    in charge. The source database is never opened for writing.

## 11. Normalized cross-language comparison

Compatibility fixtures compare TypeScript and Rust outputs. Normalization
applies before comparison and must preserve equality relationships:

- **Timestamps**: any string matching RFC 3339 with `Z` is replaced by
  `<ts>`; distinct source values may collapse.
- **Paths**: the fixture workspace root is replaced by `<workspace>`
  wherever it appears; separators are kept as-is per platform expectation.
- **IDs**: `evt_…`, `cor_…`, `ses_…`, `prj_…`, `art_…`, `run_…` values are
  replaced by `<id:kind>` placeholders through a per-fixture map so equal
  inputs map to equal placeholders and different ids stay different.
- **Maps**: JSON objects are compared key-order-insensitively (parse, then
  compare sorted structures).
- **Durations**: `durationMs` values are replaced by `<ms>`.
- **Artifacts**: compared byte-for-byte except that frontmatter fields
  covered by the ID/timestamp rules are normalized the same way.
- **Everything else** — strings, numbers, enums, ordering of arrays and
  event sequences — is compared exactly.

Fixture generation must be deterministic: fixed injected clocks and seeded
ids during generation, so most fixtures need no normalization at all.

## 12. Versioning and compatibility summary

- `envelopeVersion` governs the container; `storeVersion` in the manifest
  mirrors it. Both are `1`.
- Per-type payload `schemaVersion` values continue to follow
  `EVENT_PAYLOAD_VERSIONS` from the TypeScript contracts.
- The minimum Relay version able to read a store is recorded in
  `manifest.minReaderVersion`.
- Unknown event types and newer versions always fail closed with upgrade
  guidance; additive optional payload fields (such as `role` on model
  events) must not invalidate older records.
- Events never contain secrets, credentials, API keys, or authorization
  headers. Redaction happens before any event is appended.
