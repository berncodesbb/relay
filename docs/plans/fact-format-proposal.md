# Fact format proposal (intent-first work graph persistence)

Status: accepted (lead review 2026-10-13) — normative for the intent-first
fact store. Decisions on §9: (1) root stays `.relay-graph` for the
preview era; (2) checksums permanent, chained mode stays implemented but
off; (3) id-free envelope + single recordedAt confirmed; (4) plain-notation
float subset confirmed; (5) domain validator + graph fold land with the
domain contract. See `docs/plans/intent-first-execution-log.md`.
Date: 2026-10-13
Owners: storage/migration (format + implementation), domain owner (fact
catalog, payload schemas), lead integrator (acceptance)
Implementation: `crates/relay-store/src/fact/**` — the generic layer plus
(wave 2) the domain seam (`fact::domain`: `DomainFactValidator` over the
`relay-domain` fact catalog, `GraphFactStore`), the graph projection fold
(`fact::graph`), and the doctor services (`fact::doctor`). Domain fact
types themselves remain the domain owner's contract in `relay-domain`.

This document specifies the durable record format for the intent-first
work graph: the fact envelope, canonicalization, integrity, segmentation,
projection/rebuild semantics, crash-recovery points, and (outline only)
how legacy projects map onto it. `docs/event-format.md` remains normative
for the frozen legacy event path and is the reference for the principles
reused here; nothing in this proposal changes it.

## 1. Authority order

```text
canonical JSONL facts -> deterministic replay -> work-graph state
                    -> SQLite projections -> efficient queries
                    -> Markdown views -> human review
```

If JSONL and SQLite disagree, JSONL wins and the projection is repaired or
rebuilt. Deleting `projection.sqlite` between runs must always produce an
equivalent database. Facts are written and fsynced before any operation
reports success and before any projection row is committed.

## 2. Fact envelope

Each durable domain change is one JSON object on one physical line in a
segmented JSONL log:

```json
{
  "checksum": "9f1c…64 lowercase hex…",
  "factVersion": 1,
  "payload": { },
  "previous": null,
  "recordedAt": "2026-10-13T08:00:00.123Z",
  "sequence": 42,
  "type": "goal.declared"
}
```

| Field | Type | Rules |
| --- | --- | --- |
| `sequence` | integer | 1-based, contiguous, increasing by exactly 1 across the whole stream. The record's identity within the stream: there is no separate record id, and no `projectId` per record — one log is one stream, named once in the manifest. |
| `type` | string | Non-empty, domain-owned open string (e.g. `goal.declared`, `receipt.accepted`). The storage layer never interprets it; the domain's `FactValidator` decides acceptance so unknown types fail closed with domain-worded upgrade guidance. |
| `factVersion` | integer | Per-type payload version, ≥ 1. Acceptance is the domain's gate: a version the domain does not know fails closed (see §3). |
| `payload` | object | Domain payload. Must be a JSON object (never a bare scalar/array) so additive envelope-level evolution and payload keyed lookups stay uniform. |
| `recordedAt` | string | RFC 3339 UTC, millisecond precision, `Z` suffix — when storage durably recorded the fact. All domain semantics of when something *happened* (attempt start/end, decision made) belong in the payload, where the domain owns the schema; the envelope carries exactly one storage-owned timestamp. |
| `previous` | string \| null | Lowercase hex SHA-256 of the previous record's `checksum`; present **only** when the stream records `integrity: "chained"` and `sequence > 1` (§5). Always `null` on a `checksums` stream. |
| `checksum` | string | Lowercase hex SHA-256 over the canonical serialization of this envelope minus the `checksum` field itself (§4). |

### Versioning policy

- **One container/record version**: the manifest's `factStoreVersion`
  (currently `1`) governs both the container (segments, manifest fields)
  and the record line format. There is no per-record `envelopeVersion`
  field — the manifest is always present and always read first. Readers
  that see a newer `factStoreVersion` fail closed with upgrade guidance.
- **Additive fields do not invalidate**: readers accept unknown
  additional fields in the envelope, the manifest, and (per domain
  policy) payloads, and drop them on decode. The checksum is computed
  over the known fields only, so additive fields never break digest
  verification.
- **Newer/unknown fails closed**: a newer `factStoreVersion`, a newer
  `factVersion`, or an unknown `type` is never silently skipped. The
  first two are hard storage errors; the latter is the domain gate's
  rejection. Removal or retyping of an existing field is a *new*
  `factStoreVersion` (or `factVersion`) and requires a migration.

## 3. Line framing and canonicalization

Canonical serialization is owned by the storage crate
(`relay_store::fact::canonical`) and used for (a) the bytes written into
JSONL segments and (b) the checksum input. Readers re-serialize after
parsing and never hash raw input bytes.

1. UTF-8 throughout; no BOM; no insignificant whitespace.
2. Object keys sorted lexicographically by Unicode code point (equival:
   byte-wise over UTF-8), recursively.
3. Strings escaped minimally: `"` → `\"`, `\` → `\\`, controls as
   `\b \f \n \r \t` or `\u00xx` (lowercase hex) for the remaining C0
   range; non-ASCII emitted as raw UTF-8.
4. `true`, `false`, `null` as the JSON literals.
5. One record per physical line, each line terminated by `\n` (LF).
   A line never contains a raw LF.

**Supported number subset** (deliberately simpler than the legacy
JavaScript-compatible float layout; this format has no cross-language
byte-parity constraint):

- Integers: exact decimal, range `i64` ∪ `u64` (≤ `u64::MAX`); no
  exponent, no leading zeros, no `+`.
- Non-integers: finite IEEE-754 binary64, serialized as the shortest
  decimal that round-trips, **always in plain positional notation** —
  never exponential. `1e21` serializes as `1000000000000000000000`,
  `1e-7` as `0.0000001`, `2.0` as `2`, `-0.0` as `-0`.
- `NaN` / `±∞` are unrepresentable: parsers never produce them and the
  serializer rejects them.
- Domains needing exact decimals (money-like values) must encode them as
  integers with a documented scale or as strings.

## 4. Integrity: checksum by default, chain opt-in

**Decision: per-record SHA-256 checksum is the default and permanent
integrity mechanism. A per-stream hash chain (`previous` links) is
implemented but OFF by default** (`integrity: "checksums"` in the
manifest; `"chained"` opts in per store at creation and is recorded
permanently in the manifest).

Rationale:

- The checksum detects the accident class that matters for a local-first
  tool: partial writes, bit rot, truncated files, and accidental edits of
  a single record. It also pins the exact bytes a fact occupied, which
  the projection checkpoint reconciles against.
- A chain additionally detects **reordering and selective truncation that
  preserves every surviving record's own checksum** — i.e. an adversary
  or a surgical editor removing/reordering history. The legacy format's
  own spec (§3) states the chain "is not an authentication mechanism
  against a malicious local user": anyone able to rewrite history can
  recompute the chain too. Its real marginal value is detecting
  non-malicious surgical edits, which the segment-tiling rules plus
  per-record checksums already constrain heavily.
- A chain costs rewriting risk: it couples every record to its
  predecessor's digest, complicates future compaction/retention
  (§6), cross-stream merges, and parallel-writer protocols, for a threat
  the plan explicitly says needs "a distinct product/security
  justification" before becoming part of the permanent format.
- Segment-level integrity stays regardless: closed segments must tile the
  sequence space contiguously, end with `\n`, and hold exactly the record
  count their name promises; discovery is deterministic; unknown files in
  `facts/` are reported and never silently consumed.

Recommendation to the lead: keep `checksums` as the shipped default; treat
`chained` as an available, tested mode for users or future products that
want tamper-evidence of *reordering* and can state that justification. If
no such justification arrives, remove the chained mode at the retirement
gate rather than carrying it silently.

## 5. Segments, rotation, and retention boundaries

Layout (default root `<workspace>/.relay-graph`, parameterizable via
`FactPaths::from_root` for migration staging):

```text
.relay-graph/
  manifest.json
  facts/
    0000000000000001-00000000000003e8.jsonl
    00000000000003e9-open.jsonl
  projection.sqlite
  locks/writer.lock
  recovery/rejected-fact-tail-20261013T080000Z.jsonl
```

- Segment naming, discovery, tiling validation, and rotation thresholds
  are **reused unchanged** from the legacy store: 16-digit hex names,
  `<first>-<last>.jsonl` closed / `<first>-open.jsonl` active, rotation
  after a durable append at ≥ 1000 records or ≥ 8 MiB.
- The default root is distinct from the legacy `.relay` root so the
  frozen event store and the fact store can coexist in one workspace
  during the migration window with no possibility of one interpreting
  the other's segments, manifest, or projection. The semantic migration
  (Phase 4) renames directories atomically at its final switch, the same
  technique the legacy importer uses.

**Retention boundary — operational stream data never becomes a domain
fact.** Provider stream deltas, full process transcripts, transport
diagnostics, tool stdout beyond the bounded redacted summary, and
scheduler telemetry are *not* durable domain facts. They are bounded,
rotating operational logs outside `.relay-graph/facts` (owned by the
runtime layer), retained only as referenced evidence blobs when a fact
explicitly carries their digest and path. A fact references evidence by
`(path, sha256, bytes)` and never inlines large content. Consequence:
the fact log stays small enough that whole-log replay remains the
rebuild mechanism; no compaction is required for the first release, and
if one is ever added, it is a `factStoreVersion` bump with a migration.

## 6. SQLite projection, checkpoint, rebuild

`projection.sqlite` is derived, disposable state. The storage layer owns:

- a generic `facts` table — sequence (PK), type, factVersion, recordedAt,
  previous, checksum, canonical payload text — indexed for type and time
  queries;
- a one-row `fact_checkpoint` — last indexed sequence + checksum;
- forward-only migrations recorded in `fact_schema_migrations`
  (independent of `factStoreVersion`).

Domain-specific folds plug in through `FactFold::apply(&Transaction,
&FactRecord)` (plus `prepare` for DDL) and run **inside the same
transaction** as the storage rows and the checkpoint, in chunks of 500
records. Every committed transaction therefore describes exactly one
prefix of the log; a crash at any commit boundary converges on reopen.

Reconciliation on open — checkpoint equals tail → matched; behind →
suffix replay; missing/corrupt/ahead/divergent/newer-schema → quarantine
into `recovery/` and rebuild entirely from JSONL. The graph projection
(normalized work/edge/decision/artifact/evidence/attempt/invalidation
tables for lookup and search) is a domain fold over this primitive,
implemented in `relay_store::fact::graph` against the frozen
`relay-domain` contract: the fold decodes each record through
`fact_from_parts`, so an undecodable payload fails closed. **Readiness,
blockers, and awaiting-human are never projection rows** — the app derives
them with `relay-engine` over the graph folded from JSONL
(`GraphFactStore::project_graph`). Deleting the database between runs must
always produce an equivalent one — verified by
`relay-tests/tests/fact_replay.rs` for both a toy fold and a real
`relay-domain` fact stream.

The writer lock (`locks/writer.lock`, stale-PID detection, bounded
acquisition timeout) is held only for storage operations: never across
model calls or long-running tools.

## 7. Append protocol and crash-recovery points

Exactly one process appends at a time. Protocol: acquire the lock →
refresh the tail → seal and domain-validate the whole batch (sequence,
recordedAt from the injected clock, chain link when chained, checksum) →
one buffered write → flush + fsync (`Durable`; `Relaxed` exists for
tests) → rotate if thresholds crossed → fold the projection → release
the lock. **The user-visible success boundary is the durable JSONL
append.** A projection failure after it is reported as a lagging index;
the next open replays the suffix.

Through the domain seam (`GraphFactStore`), the protocol additionally runs
a **domain fold gate**: the whole candidate history (existing facts plus
the batch) must fold through `ProjectGraph::from_facts` before anything is
written — duplicates, unknown references, cycles, capability violations,
and sequence violations abort with the domain's error and the store stays
untouched. The gate runs outside the writer lock against the timeline read
at the start of the call, and the append then **pins that revision**: a
concurrent writer turns into a stale-revision refusal rather than an
unvalidated commit. Checked-by-default is a property of the typed
(`GraphFactStore`) surface; the envelope-only path stays internal to the
generic log/recovery layer (`FactStore`), which the importer and
reconciliation machinery use directly.

Fault-injection points (reused from the legacy machinery, covered for the
fact path in `fact_replay.rs`): `LockAcquired`, `BatchValidated`,
`BeforeWrite`, `AfterPartialWrite`, `AfterWriteNoSync`,
`AfterDurableAppend`, `AfterRotation`, `ProjectionChunkCommitted`,
`ProjectionCommitted`. Contract: any fact whose append returned success
survives every crash after it; facts never durably written are never
resurrected; every crash point converges on reopen.

Tail recovery is conservative and unchanged in spirit: the only automatic
repair is an incomplete final tail of the **active** segment (file not
ending in `\n`). In writer mode the incomplete bytes are copied to
`recovery/rejected-fact-tail-<stamp>Z.jsonl` and the file is truncated to
the last valid newline; readers take the consistent prefix and ignore the
tail. A closed segment with an incomplete final line, or an incomplete
tail followed by more data, fails the open. Any complete-but-invalid
record (bad checksum, sequence gap, chain break, domain rejection) fails
the open with segment, line, sequence, and reason — complete invalid
history is never silently removed.

## 8. Legacy mapping (Phase 4 — implemented in `relay_store::migrate`)

The semantic migration is implemented: read-only evidence readers for
`.relay/state.db` and old JSONL event stores (validated through relay-core,
the frozen reference decoder, isolated to the importer path), pure meaning
extraction into domain facts with a structured report (mappings, unmapped
kinds with counts, coarse inferences, needs-review items), staging into a
sibling root, a validation gate (the staged stream must fold through the
domain graph AND pass the fact doctor before the switch), an atomic
two-rename switch retaining any previous store, and rollback. Verified by
`relay-tests/tests/semantic_migration.rs` against the real exported corpus
plus relay-core-generated legacy bytes.

- **`.relay/state.db` (TypeScript projects)**: read-only import. Replay
  the legacy event stream through the legacy reducer *as evidence*, then
  extract durable product meaning — goal, approvals, artifact versions
  and their bytes/digests, requirements, criteria, decisions, tasks,
  evidence, delivery status — into **new** domain facts (the domain owner
  defines the mapping table). Legacy events with no durable meaning
  (mode churn, interrupted runs, model call diagnostics) are reported as
  unmapped, not fabricated. Stage into a fresh `.relay-graph` root via
  `FactPaths::from_root`, validate the rebuilt graph, then switch by
  atomic rename with the original retained as backup. Never write to the
  source; rollback restores it.
- **Old Rust JSONL event stores** (`docs/event-format.md` format): same
  treatment — the JSONL is already segmented and validated; the importer
  replays it and emits facts. The old `events/` tree is *not* adopted
  as the fact log; a fact log must contain only fact records.
- Coarse legacy relationships become coarse dependencies marked
  `imported/coarse`; precise dependencies are never invented.
- The legacy importer's staging/backup/rollback rules
  (quarantine-first, atomic switch, no automatic deletion) carry over
  unchanged.

## 9. Decisions requested from the lead

1. Default root `.relay-graph` (vs. a subdirectory of `.relay`). The
   distinct root is recommended for the migration window; changing it
   later is one line in `FactPaths::new`.
2. Confirm checksum-not-chain as the permanent default (§4).
3. Confirm the envelope stays id-free (sequence is the identity; domain
   ids live in payloads) and carries exactly one storage timestamp.
4. Confirm the number subset (plain-notation floats, `u64`-bounded
   integers) for the new format.
5. Domain owner to supply the real `FactValidator` (fact catalog +
   versions) and the graph `FactFold`; until then the generic layer ships
   with `AcceptAllFacts` (tests only — production callers must inject
   their gate so unknown types fail closed).

   *Delivered (wave 2):* `relay_domain::facts::{KNOWN_FACT_KINDS,
   current_fact_version, fact_to_parts, fact_from_parts}` is the catalog;
   `relay_store::fact::DomainFactValidator` and
   `relay_store::fact::graph::GraphFold` implement the two seams, and
   `GraphFactStore` uses them exclusively — `AcceptAllFacts` is unreachable
   from the production types.
