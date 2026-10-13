#!/usr/bin/env node
/**
 * Compatibility fixture exporter (Rust rewrite, phase 0).
 *
 * Generates the deterministic, sanitized, versioned corpus under
 * `tests/fixtures/` that pins the TypeScript implementation's behavior as the
 * compatibility oracle for the Rust port:
 *
 * - `tests/fixtures/cases/<name>/` — one directory per event-log case. Valid
 *   cases carry `events.jsonl` (canonical, hash-chained lines per
 *   `docs/event-format.md` §1–§3 with envelopeVersion 1), `expected-state.json`
 *   (canonical `replayProject` output), `expected-status.json` and
 *   `expected-why.json` (canonical controller views), and `meta.json`. Invalid
 *   cases carry `events.jsonl` plus a `meta.json` describing the exact expected
 *   failure (error class name and message substring).
 * - `tests/fixtures/config/<name>/` — legacy inline-route and role-based YAML
 *   inputs plus the expected normalized routing captured from the TypeScript
 *   normalization path (`normalizeRouting`).
 * - `tests/fixtures/cli/` — help text, version, and exit-code mapping.
 *
 * Determinism: every id, timestamp, and duration is constructed here (or, for
 * the end-to-end run, normalized per §11 through an equality-preserving
 * per-fixture placeholder map), so running the exporter twice produces
 * byte-identical output. The corpus contains no secrets, credentials, local
 * user paths, or live transcripts.
 *
 * Usage: node scripts/export-compatibility-fixtures.mjs [--out <dir>]
 */

import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

// ---------------------------------------------------------------------------
// tsx bootstrap: this script imports TypeScript sources directly (the current
// implementation is the oracle), so when launched without a TS loader it
// re-executes itself under tsx exactly the way test/integration/cli.test.ts
// drives the CLI. RELAY_FIXTURES_CHILD prevents a spawn loop.
// ---------------------------------------------------------------------------
if (process.env.RELAY_FIXTURES_CHILD !== '1') {
  const require = createRequire(import.meta.url);
  let tsx;
  try {
    tsx = require.resolve('tsx');
  } catch {
    console.error('relay fixtures: the tsx dev dependency is required (run npm install).');
    process.exit(1);
  }
  const result = spawnSync(
    process.execPath,
    ['--import', tsx, fileURLToPath(import.meta.url), ...process.argv.slice(2)],
    { stdio: 'inherit', env: { ...process.env, RELAY_FIXTURES_CHILD: '1' } },
  );
  process.exit(result.status ?? 1);
}

import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative } from 'node:path';
import { parse as parseYaml, stringify as stringifyYaml } from 'yaml';
import {
  canonicalize,
  toCanonicalLine,
  verifyCanonicalLog,
  withHashChain,
} from './lib/canonical-json.mjs';

// TypeScript-source imports (the oracle). These MUST stay dynamic: static
// imports are hoisted above the tsx bootstrap above and would fail to resolve
// under plain `node scripts/...`.
const { DEFAULT_CONFIG } = await import('../src/config/defaults.js');
const { loadConfig, normalizeRouting } = await import('../src/config/load.js');
const { parseRelayConfig } = await import('../src/config/schema.js');
const { EVENT_PAYLOAD_VERSIONS, RELAY_EVENT_TYPES, validateStoredEvent } = await import(
  '../src/core/events.js'
);
const { replayProject } = await import('../src/core/reducer.js');
const { ArtifactMarkdownStore } = await import('../src/artifacts/markdown-store.js');
const { WorkflowController } = await import('../src/runtime/controller.js');
const { buildStatusView, buildWhyView } = await import('../src/runtime/controller-views.js');
const { createDemoFakeProvider, DEMO_GOAL } = await import('../src/runtime/demo-provider.js');
const { ModelRouter } = await import('../src/runtime/model-router.js');
const { openDatabase } = await import('../src/storage/database.js');
const { SqliteEventStore } = await import('../src/storage/event-store.js');
const { runMigrations } = await import('../src/storage/migrations.js');
const { relayPaths } = await import('../src/storage/paths.js');
const { ProjectRepository } = await import('../src/storage/project-repository.js');
const { createDefaultToolRegistry } = await import('../src/tools/index.js');

const REPO_ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const DEFAULT_OUT = join(REPO_ROOT, 'tests', 'fixtures');
const FIXTURE_FORMAT_VERSION = 1;
const GENERATED_BY = 'scripts/export-compatibility-fixtures.mjs';
const SPEC = 'docs/event-format.md (store format version 1)';

/** Fixed base instant for every generated timestamp (2026-10-13T00:00:00.000Z). */
const BASE_MS = Date.parse('2026-10-13T00:00:00.000Z');
/** Deterministic project/session identity for hand-built cases. */
const PROJECT_ID = 'prj_00000000-0000-4000-8000-000000000001';
const SESSION_ID = 'ses_00000000-0000-4000-8000-000000000001';
const GOAL = 'Ship a workflow CLI that preserves product intent';
/** Placeholder the end-to-end run's temp workspace root is normalized to. */
const WORKSPACE_PLACEHOLDER = '/relay-fixture-workspace';
const DELIVERY_HOLD = /delivery actions require explicit authorization/i;

/**
 * Deterministic uuid-v4-shaped tail: `n` rendered as 12 lowercase hex digits.
 * Combined with the `00000000-0000-4000-8000-` stem this always matches the
 * `<prefix>_<uuid-v4-lowercase>` envelope id rule.
 */
function uuidTail(n) {
  return n.toString(16).padStart(12, '0');
}

function isoAt(ms) {
  return new Date(BASE_MS + ms).toISOString();
}

// ---------------------------------------------------------------------------
// Hand-built event payloads
// ---------------------------------------------------------------------------

const artifactId = (n) => `art_00000000-0000-4000-8000-${uuidTail(n)}`;

const initialized = (autonomy = 'workflow', goal = GOAL) => ({
  type: 'project.initialized',
  payload: { goal, workspaceRoot: WORKSPACE_PLACEHOLDER, initialAutonomy: autonomy },
});

const sessionStarted = (sessionId = SESSION_ID, interactive = true) => ({
  type: 'session.started',
  payload: { sessionId, interactive },
});

const intentProposed = (proposal, source = 'command', raw = '/auto workflow') => ({
  type: 'intent.proposed',
  payload: { source, raw, proposal },
});

const intentApplied = (kind, confidence, extra = {}, source = 'command', overridden = false) => ({
  type: 'intent.applied',
  payload: {
    kind,
    source,
    confidence,
    mode: extra.mode ?? null,
    autonomy: extra.autonomy ?? null,
    artifactKind: extra.artifactKind ?? null,
    overridden,
  },
});

const modeEntered = (to, from, source = 'auto', reason = `${to} approved`) => ({
  type: 'mode.entered',
  payload: { from, to, reason, source },
});

const transitionRejected = (from, to, reason) => ({
  type: 'mode.transition.rejected',
  payload: { from, to, reason },
});

const autonomyChanged = (from, to, source = 'user') => ({
  type: 'autonomy.changed',
  payload: { from, to, source },
});

const artifactCreated = (kind, artId, title, n) => ({
  type: 'artifact.created',
  payload: {
    artifactId: artId,
    kind,
    title,
    version: 1,
    path: `.relay/artifacts/${kind}/v1-fixture-${uuidTail(n ?? 1)}.md`,
    summary: `fixture ${kind} summary`,
  },
});

const artifactApprovalRequested = (artId, kind, version = 1) => ({
  type: 'artifact.approval.requested',
  payload: { artifactId: artId, kind, version, reason: `ready for review: ${kind}` },
});

const artifactApproved = (artId, kind, version = 1) => ({
  type: 'artifact.approved',
  payload: { artifactId: artId, kind, version, approvedBy: 'user' },
});

const artifactRevisedTo = (artId, kind, fromVersion, toVersion, n, reason) => ({
  type: 'artifact.revised',
  payload: {
    artifactId: artId,
    kind,
    fromVersion,
    toVersion,
    path: `.relay/artifacts/${kind}/v${toVersion}-fixture-${uuidTail(n ?? 1)}.md`,
    reason,
  },
});

const artifactRevised = (artId, kind, fromVersion, n, reason = 'fixture revision') =>
  artifactRevisedTo(artId, kind, fromVersion, fromVersion + 1, n, reason);

const artifactInvalidated = (artifactIds, cause) => ({
  type: 'artifact.invalidated',
  payload: { artifactIds, cause, reason: 'upstream revision' },
});

const artifactInspected = (artId, via = 'user') => ({
  type: 'artifact.inspected',
  payload: { artifactId: artId, via },
});

const transitionEvaluated = (outcome, extra, reason, evidence) => ({
  type: 'transition.evaluated',
  payload: {
    outcome,
    to: extra.to ?? null,
    reviseStage: extra.reviseStage ?? null,
    reason,
    evidence,
  },
});

const modelRequested = (
  purpose,
  mode,
  attempt = 1,
  role,
  provider = 'anthropic',
  model = 'claude-sonnet-5-5',
) => ({
  type: 'model.requested',
  payload:
    role === undefined
      ? { provider, model, purpose, mode, attempt }
      : { provider, model, purpose, mode, attempt, role },
});

const modelResponded = (
  purpose,
  role,
  usage = { inputTokens: 100, outputTokens: 50 },
  toolCalls = 0,
  durationMs = 25,
  provider = 'anthropic',
  model = 'claude-sonnet-5-5',
) => ({
  type: 'model.responded',
  payload:
    role === undefined
      ? { provider, model, purpose, usage, toolCalls, durationMs }
      : { provider, model, purpose, role, usage, toolCalls, durationMs },
});

const modelFailed = (
  purpose,
  errorKind = 'rate-limit',
  message = 'throttled after retries',
  attempts = 3,
  durationMs = 900,
  role,
  provider = 'anthropic',
  model = 'claude-sonnet-5-5',
) => ({
  type: 'model.failed',
  payload:
    role === undefined
      ? { provider, model, purpose, errorKind, message, attempts, durationMs }
      : { provider, model, purpose, role, errorKind, message, attempts, durationMs },
});

const toolExecuted = (
  tool,
  capability,
  mode,
  outcome = 'ok',
  durationMs = 12,
  summary = 'read repository files',
  affectedFiles = ['README.md'],
) => ({
  type: 'tool.executed',
  payload: { tool, capability, mode, outcome, durationMs, summary, affectedFiles },
});

const toolDenied = (tool, capability, mode, reason) => ({
  type: 'tool.denied',
  payload: { tool, capability, mode, reason },
});

const verificationCompleted = (passed, reportArtifactId) => ({
  type: 'verification.completed',
  payload: {
    passed,
    results: [
      {
        criterion: 'AC-1: workflow renders one artifact per mode',
        status: passed ? 'pass' : 'fail',
        evidence: 'artifact files exist under .relay/artifacts',
      },
      { criterion: 'AC-2: workflow pauses before delivery', status: 'pass', evidence: 'policy pause recorded' },
      { criterion: 'AC-3: no user commits', status: 'unevaluated', evidence: '' },
    ],
    reportArtifactId,
  },
});

const workflowPaused = (by, reason, question = null, options = []) => ({
  type: 'workflow.paused',
  payload: { by, reason, question, options },
});

const workflowResumed = (note = 'resuming after review') => ({
  type: 'workflow.resumed',
  payload: { by: 'user', note },
});

const budgetExhausted = (budget = 'modelCalls', limit = 48, detail = 'model call limit reached') => ({
  type: 'budget.exhausted',
  payload: { budget, limit, detail },
});

const runInterrupted = (phase = 'implementation', hint = 'model stream canceled (Ctrl-C)') => ({
  type: 'run.interrupted',
  payload: { phase, hint },
});

/** Artifact ids used across hand-built cases (stable, uuid-shaped). */
const ART = {
  brief: artifactId(0x101),
  prd: artifactId(0x102),
  trd: artifactId(0x103),
  plan: artifactId(0x104),
  log: artifactId(0x105),
  vr: artifactId(0x106),
  dr: artifactId(0x107),
};

/**
 * The full approved pipeline with approval requests and telemetry: every
 * contractual artifact created, requested, and approved; every mode entered in
 * order; a verification run before delivery; the delivery report produced and
 * the workflow held at the delivery pause.
 */
function fullPipeline(options = {}) {
  const verificationPassed = options.verificationPassed ?? true;
  const events = [
    initialized(),
    sessionStarted(),
    artifactCreated('opportunity-brief', ART.brief, 'Fixture opportunity brief', 1),
    artifactApprovalRequested(ART.brief, 'opportunity-brief'),
    modelRequested('mode:discovery', 'discovery', 1, 'reasoning'),
    modelResponded('mode:discovery', 'reasoning'),
    artifactApproved(ART.brief, 'opportunity-brief'),
    transitionEvaluated('ADVANCE', { to: 'specification' }, 'opportunity-brief approved', ['approved v1']),
    modeEntered('specification', 'discovery', 'auto', 'brief approved'),
    artifactCreated('prd', ART.prd, 'Fixture PRD', 2),
    artifactApprovalRequested(ART.prd, 'prd'),
    modelRequested('mode:specification', 'specification', 1, 'reasoning'),
    modelResponded('mode:specification', 'reasoning'),
    artifactApproved(ART.prd, 'prd'),
    transitionEvaluated('ADVANCE', { to: 'technical-design' }, 'prd approved', ['approved v1']),
    modeEntered('technical-design', 'specification', 'auto', 'prd approved'),
    artifactCreated('trd', ART.trd, 'Fixture TRD', 3),
    artifactApprovalRequested(ART.trd, 'trd'),
    modelRequested('mode:technical-design', 'technical-design', 1, 'architect'),
    modelResponded('mode:technical-design', 'architect'),
    artifactApproved(ART.trd, 'trd'),
    transitionEvaluated('ADVANCE', { to: 'planning' }, 'trd approved', ['approved v1']),
    modeEntered('planning', 'technical-design', 'auto', 'trd approved'),
    artifactCreated('execution-plan', ART.plan, 'Fixture execution plan', 4),
    artifactApprovalRequested(ART.plan, 'execution-plan'),
    modelRequested('mode:planning', 'planning', 1, 'reasoning'),
    modelResponded('mode:planning', 'reasoning'),
    artifactApproved(ART.plan, 'execution-plan'),
    transitionEvaluated('ADVANCE', { to: 'implementation' }, 'execution-plan approved', ['approved v1']),
    modeEntered('implementation', 'planning', 'auto', 'plan approved'),
    artifactCreated('decision-log', ART.log, 'Fixture decision log', 5),
    toolExecuted('apply-patch', 'write', 'implementation', 'ok', 40, 'patched src/greet.js', ['src/greet.js']),
    modeEntered('verification', 'implementation', 'auto', 'implementation complete'),
    artifactCreated('verification-report', ART.vr, 'Fixture verification report', 6),
    verificationCompleted(verificationPassed, ART.vr),
  ];
  if (verificationPassed) {
    events.push(
      artifactApprovalRequested(ART.vr, 'verification-report'),
      artifactApproved(ART.vr, 'verification-report'),
      transitionEvaluated('ADVANCE', { to: 'delivery' }, 'verification-report approved', ['approved v1']),
      modeEntered('delivery', 'verification', 'auto', 'verification approved'),
      artifactCreated('delivery-report', ART.dr, 'Fixture delivery report', 7),
      transitionEvaluated('NEEDS_USER', {}, 'delivery actions require explicit user authorization', [
        'delivery report produced',
      ]),
    );
  }
  return events;
}

// ---------------------------------------------------------------------------
// Case definitions
// ---------------------------------------------------------------------------

/** Valid hand-built cases: name, description, and event specs. */
const VALID_CASES = [
  {
    name: 'initialized-only',
    description: 'A freshly initialized project: one project.initialized event and nothing else.',
    specs: () => [initialized()],
  },
  {
    name: 'all-event-types',
    description:
      'A kitchen-sink tour that folds every one of the 24 event types exactly once into a single valid log.',
    specs: () => [
      initialized(),
      sessionStarted(),
      intentProposed(
        {
          kind: 'set-autonomy',
          confidence: 0.98,
          rationale: 'user asked for workflow autonomy',
          autonomy: 'workflow',
        },
        'command',
        '/auto workflow',
      ),
      intentApplied('set-autonomy', 0.98, { autonomy: 'workflow' }),
      artifactCreated('opportunity-brief', ART.brief, 'Tour opportunity brief', 1),
      artifactApprovalRequested(ART.brief, 'opportunity-brief'),
      modelRequested('mode:discovery', 'discovery', 1, 'reasoning'),
      modelResponded('mode:discovery', 'reasoning'),
      toolExecuted('search', 'read', 'discovery'),
      toolDenied('run-command', 'execute', 'discovery', 'mode discovery permits only read tools'),
      artifactInspected(ART.brief, 'user'),
      workflowPaused(
        'policy',
        'approval needed',
        'Approve the opportunity-brief (v1) to continue',
        ['approve', 'request changes'],
      ),
      artifactApproved(ART.brief, 'opportunity-brief'),
      transitionEvaluated('ADVANCE', { to: 'specification' }, 'opportunity-brief approved', ['approved v1']),
      modeEntered('specification', 'discovery'),
      artifactCreated('prd', ART.prd, 'Tour PRD', 2),
      artifactApprovalRequested(ART.prd, 'prd'),
      artifactApproved(ART.prd, 'prd'),
      modeEntered('technical-design', 'specification'),
      artifactCreated('trd', ART.trd, 'Tour TRD', 3),
      artifactApproved(ART.trd, 'trd'),
      modeEntered('planning', 'technical-design'),
      artifactCreated('execution-plan', ART.plan, 'Tour execution plan', 4),
      artifactApproved(ART.plan, 'execution-plan'),
      modeEntered('implementation', 'planning'),
      artifactCreated('decision-log', ART.log, 'Tour decision log', 5),
      modeEntered('verification', 'implementation'),
      artifactCreated('verification-report', ART.vr, 'Tour verification report', 6),
      verificationCompleted(true, ART.vr),
      artifactApproved(ART.vr, 'verification-report'),
      modeEntered('delivery', 'verification'),
      artifactCreated('delivery-report', ART.dr, 'Tour delivery report', 7),
      artifactRevised(ART.prd, 'prd', 1, 8, 'tour revision: requirements changed'),
      artifactInvalidated([ART.trd, ART.plan], { kind: 'prd', artifactId: ART.prd, version: 2 }),
      autonomyChanged('workflow', 'until:delivery'),
      transitionRejected(
        'delivery',
        'discovery',
        'workflow modes run in order: cannot jump from delivery to discovery',
      ),
      transitionEvaluated('REVISE', { reviseStage: 'specification' }, 'prd revised; routing back', ['prd v2']),
      workflowPaused('user', 'reviewing the revision'),
      workflowResumed(),
      modelFailed('mode:delivery', 'rate-limit', 'throttled after retries', 3, 900, 'fast'),
      budgetExhausted(),
      runInterrupted('delivery'),
    ],
  },
  {
    name: 'full-pipeline-approved',
    description:
      'Every workflow mode entered in order with each contractual artifact created, approval-requested, and approved; verification passes and delivery holds.',
    specs: () => fullPipeline(),
  },
  {
    name: 'approval-gate-pause',
    description:
      'The discovery approval gate: the brief is produced, approval is requested, the workflow pauses by policy with a focused question, the user inspects and approves, and the workflow advances.',
    specs: () => [
      initialized('next-checkpoint'),
      sessionStarted(),
      artifactCreated('opportunity-brief', ART.brief, 'Gate opportunity brief', 1),
      artifactApprovalRequested(ART.brief, 'opportunity-brief'),
      workflowPaused(
        'policy',
        'approval needed',
        'Approve the opportunity-brief (v1) to continue',
        ['approve', 'request changes'],
      ),
      artifactInspected(ART.brief, 'user'),
      artifactApproved(ART.brief, 'opportunity-brief'),
      modeEntered('specification', 'discovery'),
    ],
  },
  {
    name: 'artifact-revision-invalidation',
    description:
      'Revising the approved PRD moves it back to draft v2 and marks every transitive dependent (trd, execution-plan, verification-report, delivery-report) stale.',
    specs: () => [...fullPipeline(), artifactRevised(ART.prd, 'prd', 1, 8, 'requirements changed')],
  },
  {
    name: 'verification-failure-delivery-hold',
    description:
      'A failing verification run blocks delivery: verification.completed records a failing criterion, the transition to delivery is rejected, and the workflow pauses by policy.',
    specs: () => [
      ...fullPipeline({ verificationPassed: false }),
      transitionEvaluated(
        'REVISE',
        { reviseStage: 'specification' },
        'verification failed; routing back to specification',
        ['AC-1 failed'],
      ),
      transitionRejected(
        'verification',
        'delivery',
        'cannot enter delivery: verification has not passed (last run: 2 passed, 1 failed, 1 unevaluated); complete the verification mode and address failing criteria first',
      ),
      workflowPaused('policy', 'verification failed: address failing criteria before delivery'),
    ],
  },
  {
    name: 'model-events-with-role',
    description:
      'Role-aware model telemetry: request, respond, and failed events carrying the routed role name.',
    specs: () => [
      initialized(),
      modelRequested('mode:discovery', 'discovery', 1, 'reasoning'),
      modelResponded('mode:discovery', 'reasoning'),
      modelRequested('intent', null, 1, 'fast', 'anthropic', 'claude-haiku-4-5'),
      modelResponded(
        'intent',
        'fast',
        { inputTokens: 20, outputTokens: 10 },
        0,
        8,
        'anthropic',
        'claude-haiku-4-5',
      ),
      modelRequested('mode:verification', 'verification', 2, 'review', 'openai', 'gpt-5.2'),
      modelFailed(
        'mode:verification',
        'server',
        'upstream 500 after retries',
        3,
        1500,
        'review',
        'openai',
        'gpt-5.2',
      ),
    ],
  },
  {
    name: 'model-events-legacy-no-role',
    description:
      'Legacy pre-role model telemetry: the same three event shapes without any role field, exactly as older logs record them.',
    specs: () => [
      initialized(),
      modelRequested('mode:discovery', 'discovery'),
      modelResponded('mode:discovery'),
      modelFailed('mode:discovery'),
    ],
  },
  {
    name: 'tools-executed-and-denied',
    description: 'Tool activity: a successful read, a failed execute, and a denied execute with the policy reason.',
    specs: () => [
      initialized(),
      sessionStarted(),
      toolExecuted('read-file', 'read', 'discovery', 'ok', 5, 'read README.md', ['README.md']),
      toolExecuted('run-command', 'execute', 'implementation', 'error', 120, 'npm test exited 1', []),
      toolDenied('run-command', 'execute', 'discovery', 'mode discovery permits only read tools'),
    ],
  },
  {
    name: 'budget-exhausted',
    description: 'Budget exhaustion pauses the workflow by policy with a reason naming the budget and limit.',
    specs: () => [initialized(), sessionStarted(), budgetExhausted('modelCalls', 48)],
  },
  {
    name: 'run-interrupted',
    description: 'An interrupted run pauses the workflow by user with the interruption hint as the reason.',
    specs: () => [initialized(), runInterrupted('implementation', 'model stream canceled (Ctrl-C)')],
  },
  {
    name: 'workflow-paused-resumed',
    description: 'A user pause with a question is later resumed; the pause state clears.',
    specs: () => [initialized(), workflowPaused('user', 'reviewing', 'Keep going?', ['yes', 'no']), workflowResumed()],
  },
];

/**
 * Invalid cases: well-formed, canonically serialized logs that must fail
 * validation or replay for one exact reason. Unless `expectError` is pinned,
 * the expected failure is captured from the TypeScript pipeline itself.
 */
const INVALID_CASES = [
  {
    name: 'empty-log',
    description: 'An empty event log cannot be replayed: the first event must be project.initialized.',
    specs: () => [],
  },
  {
    name: 'duplicate-project-initialized',
    description: 'A second project.initialized for an already-initialized project is rejected.',
    specs: () => [initialized(), sessionStarted(), initialized('workflow', 'A different goal entirely')],
  },
  {
    name: 'sequence-gap',
    description: 'A log whose second event jumps from sequence 1 to sequence 3 is rejected.',
    specs: () => [initialized(), sessionStarted()],
    sequenceOverrides: [1, 3],
  },
  {
    name: 'wrong-project-id',
    description: 'An event belonging to a different project cannot be folded into this log.',
    specs: () => [initialized(), sessionStarted()],
    projectOverrides: [undefined, 'prj_00000000-0000-4000-8000-00000000dead'],
  },
  {
    name: 'unknown-event-type',
    description: 'An event type outside the 24 known types fails validation fail-closed.',
    specs: () => [
      initialized(),
      { type: 'magic.happened', payload: { wish: 'more events' } },
    ],
  },
  {
    name: 'newer-schema-version',
    description:
      'A payload schema version newer than this build (2 for project.initialized) fails with upgrade guidance.',
    specs: () => [
      {
        ...initialized(),
        payload: { goal: '', workspaceRoot: '/tmp', initialAutonomy: 'workflow' },
      },
    ],
    schemaVersionOverrides: [2],
  },
  {
    name: 'newer-envelope-version',
    description:
      'An envelope written by a newer store format (envelopeVersion 2) must fail readers fail-closed per docs/event-format.md §1. The TypeScript store has no envelope version field and accepts this log; the expected failure is spec-defined and consumed by the Rust reader.',
    specs: () => [initialized(), sessionStarted()],
    envelopeVersion: 2,
    expectErrorName: 'InvalidEventEnvelopeError',
    expectError: 'envelopeVersion',
    specOnlyFailure: true,
  },
  {
    name: 'invalid-payload',
    description: 'A structurally invalid payload (empty goal) fails validation with field-level detail.',
    specs: () => [
      {
        ...initialized(),
        payload: { goal: '', workspaceRoot: '/tmp', initialAutonomy: 'workflow' },
      },
    ],
    expectErrorName: 'InvalidEventPayloadError',
    expectError: 'Event "project.initialized" (sequence 1) has an invalid payload',
  },
  {
    name: 'artifact-revised-version-jump',
    description:
      'An artifact.revised that jumps from version 1 to version 3 is rejected; revisions increment by exactly 1.',
    specs: () => [
      initialized(),
      artifactCreated('prd', ART.prd, 'Jump PRD', 2),
      artifactRevisedTo(ART.prd, 'prd', 1, 3, 3, 'skip a version'),
    ],
  },
  {
    name: 'artifact-approved-wrong-version',
    description:
      'Approving version 1 after the artifact was revised to version 2 is rejected; approve the current version.',
    specs: () => [
      initialized(),
      artifactCreated('prd', ART.prd, 'Wrong-version PRD', 2),
      artifactApproved(ART.prd, 'prd', 1),
      artifactRevised(ART.prd, 'prd', 1, 3, 'tighten scope'),
      artifactApproved(ART.prd, 'prd', 1),
    ],
  },
  {
    name: 'log-not-starting-with-initialized',
    description: 'A log whose first event is session.started is truncated or corrupted and rejected.',
    specs: () => [sessionStarted(), initialized()],
  },
];

// ---------------------------------------------------------------------------
// Envelope assembly, validation, and case emission
// ---------------------------------------------------------------------------

/** Build chained envelopes for a case: deterministic ids, sequences, timestamps. */
function buildEnvelopes(caseSpec) {
  const specs = caseSpec.specs();
  const envelopes = specs.map((spec, index) => {
    const sequence = caseSpec.sequenceOverrides?.[index] ?? index + 1;
    const projectId = caseSpec.projectOverrides?.[index] ?? PROJECT_ID;
    const schemaVersion =
      caseSpec.schemaVersionOverrides?.[index] ?? EVENT_PAYLOAD_VERSIONS[spec.type] ?? 1;
    return {
      id: `evt_00000000-0000-4000-8000-${uuidTail(sequence)}`,
      projectId,
      sequence,
      sessionId: spec.sessionId ?? SESSION_ID,
      type: spec.type,
      schemaVersion,
      timestamp: isoAt(sequence * 1000),
      correlationId: `cor_00000000-0000-4000-8000-${uuidTail(sequence)}`,
      causationId: spec.causationId ?? null,
      payload: spec.payload,
    };
  });
  return withHashChain(envelopes, caseSpec.envelopeVersion ?? 1);
}

/** Run the TypeScript validation + replay pipeline; return the thrown error or null. */
function tsPipelineError(envelopes) {
  try {
    const typed = envelopes.map((envelope) => validateStoredEvent(envelope));
    replayProject(typed);
    return null;
  } catch (error) {
    return error;
  }
}

/** Serialize any fixture JSON document as canonical bytes with a trailing newline. */
function canonicalDocument(value) {
  return `${canonicalize(value)}\n`;
}

function writeCaseFiles(caseDir, files) {
  rmSync(caseDir, { recursive: true, force: true });
  for (const [name, content] of files) {
    const target = join(caseDir, name);
    mkdirSync(dirname(target), { recursive: true });
    writeFileSync(target, content);
  }
}

/** Emit one hand-built case (valid or invalid). */
function emitHandBuiltCase(outDir, caseSpec, valid) {
  const envelopes = buildEnvelopes(caseSpec);
  const envelopeVersion = caseSpec.envelopeVersion ?? 1;
  const lines = envelopes.map((envelope) => toCanonicalLine(envelope));
  const eventsJsonl = envelopes.length === 0 ? '' : `${lines.join('\n')}\n`;

  const violations = verifyCanonicalLog(eventsJsonl, { expectedEnvelopeVersion: envelopeVersion });
  if (violations.length > 0) {
    throw new Error(`case "${caseSpec.name}" produced a non-canonical log: ${violations.join('; ')}`);
  }

  let meta = {
    formatVersion: FIXTURE_FORMAT_VERSION,
    name: caseSpec.name,
    description: caseSpec.description,
    valid,
    envelopeVersion,
    eventTypes: envelopes.map((envelope) => envelope.type),
    sessionKey: SESSION_ID,
    generatedBy: GENERATED_BY,
    spec: SPEC,
  };
  const files = new Map([['events.jsonl', eventsJsonl]]);

  if (valid) {
    // Valid case: validate, replay, and capture views from the TypeScript oracle.
    const typed = envelopes.map((envelope) => validateStoredEvent(envelope));
    const state = replayProject(typed);
    const status = buildStatusView(state, typed, meta.sessionKey);
    const why = buildWhyView(state, typed);
    files.set('expected-state.json', canonicalDocument(state));
    files.set('expected-status.json', canonicalDocument(status));
    files.set('expected-why.json', canonicalDocument(why));
  } else {
    let errorName = caseSpec.expectErrorName;
    let expectError = caseSpec.expectError;
    let typescriptError = null;
    if (errorName === undefined || expectError === undefined) {
      const error = tsPipelineError(envelopes);
      if (error === null) {
        throw new Error(
          `invalid case "${caseSpec.name}" replayed successfully in TypeScript; the case is not actually invalid`,
        );
      }
      errorName = error.name;
      expectError = error.message;
      typescriptError = `${error.name}: ${error.message}`;
    }
    meta = {
      ...meta,
      expectErrorName: errorName,
      expectError,
      ...(typescriptError !== null ? { typescriptError } : {}),
      ...(caseSpec.specOnlyFailure
        ? {
            specOnlyFailure: true,
            note: 'The TypeScript reference has no envelopeVersion field and accepts this log; the expected failure is defined by docs/event-format.md §1 and is asserted by the Rust reader.',
          }
        : {}),
    };
  }

  files.set('meta.json', canonicalDocument(meta));
  writeCaseFiles(join(outDir, 'cases', caseSpec.name), files);
  return meta;
}

// ---------------------------------------------------------------------------
// End-to-end demo run (fake provider, full seven-mode workflow)
// ---------------------------------------------------------------------------

/**
 * §11 normalizer for the end-to-end run. Ids map to deterministic placeholders
 * through an equality-preserving per-fixture map (equal ids map equal, distinct
 * ids stay distinct, prefixes are kept); durations collapse to zero; the temp
 * workspace path is replaced.
 *
 * Timestamps cannot use a first-appearance map: the real run records
 * millisecond timestamps whose distinct-value set jitters between runs (two
 * events sharing a millisecond in one run differ by 1 ms in the next). Each
 * event's timestamp is therefore normalized to a sequence-derived instant
 * (`base + sequence * 1000ms`, still RFC 3339 with milliseconds and Z) and each
 * artifact's frontmatter timestamps to its position in the sorted artifact
 * list. §11 explicitly allows distinct source values to collapse; the fixtures
 * stay self-consistent because state, status, and why expectations are all
 * computed from the normalized events.
 */
function createNormalizer(workspaceRoot) {
  const ids = new Map();
  let idCounter = 0;
  const ID_RE =
    /^(evt|cor|ses|session|prj|art|run)_[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

  const mapString = (value) => {
    if (ID_RE.test(value)) {
      let mapped = ids.get(value);
      if (mapped === undefined) {
        idCounter += 1;
        mapped = `${value.slice(0, value.indexOf('_') + 1)}00000000-0000-4000-8000-${uuidTail(idCounter)}`;
        ids.set(value, mapped);
      }
      return mapped;
    }
    if (workspaceRoot !== null && value.includes(workspaceRoot)) {
      return value.split(workspaceRoot).join(WORKSPACE_PLACEHOLDER);
    }
    return value;
  };

  const walk = (value) => {
    if (typeof value === 'string') return mapString(value);
    if (Array.isArray(value)) return value.map(walk);
    if (value !== null && typeof value === 'object') {
      const out = {};
      for (const [key, entry] of Object.entries(value)) {
        out[key] = key === 'durationMs' && typeof entry === 'number' ? 0 : walk(entry);
      }
      return out;
    }
    return value;
  };

  /** Normalize one raw event envelope: ids/durations/paths via walk, timestamp from sequence. */
  const normalizeEvent = (envelope) => ({
    ...walk(envelope),
    timestamp: isoAt(envelope.sequence * 1000),
  });

  /** Timestamp for the n-th artifact (sorted artifact list), in a disjoint range from events. */
  const artifactTimestamp = (n) => isoAt((10_000 + n) * 1000);

  return { walk, normalizeEvent, artifactTimestamp };
}

const DEMO_SESSION_ID = 'ses_00000000-0000-4000-8000-0000000000e1';

/** Drive the real controller with the scripted demo provider to the delivery hold. */
async function runDemoWorkflow() {
  const workspace = mkdtempSync(join(tmpdir(), 'relay-fixture-e2e-'));
  const isolatedHome = mkdtempSync(join(tmpdir(), 'relay-fixture-e2e-home-'));
  const normalizer = createNormalizer(workspace);
  try {
    const paths = relayPaths(workspace);
    const db = openDatabase(paths.database);
    runMigrations(db);
    const store = new SqliteEventStore(db);
    const repository = new ProjectRepository(db, store, replayProject);
    const artifacts = new ArtifactMarkdownStore(paths.artifactsDir);
    const tools = createDefaultToolRegistry(DEFAULT_CONFIG.tools);
    const loaded = loadConfig({
      workspaceRoot: workspace,
      userConfigPath: join(isolatedHome, '.config', 'relay', 'config.yaml'),
      overrides: { forceProvider: 'fake' },
    });
    const router = new ModelRouter(new Map([['fake', createDemoFakeProvider()]]), loaded.config);
    const controller = new WorkflowController(
      {
        workspaceRoot: workspace,
        repository,
        eventStore: store,
        artifacts,
        router,
        tools,
        budgets: DEFAULT_CONFIG.budgets,
      },
      { initialAutonomy: 'workflow', sessionId: DEMO_SESSION_ID },
    );
    await controller.initialize(DEMO_GOAL);

    let reachedDelivery = false;
    for (let index = 0; index < 60 && !reachedDelivery; index += 1) {
      const result = await controller.runStep();
      if (result.outcome.kind === 'NEEDS_USER') {
        const question = result.outcome.question;
        if (DELIVERY_HOLD.test(question)) {
          reachedDelivery = true;
        } else {
          await controller.approve();
        }
      }
    }
    if (!reachedDelivery) {
      throw new Error('demo workflow did not reach the delivery hold within the step budget');
    }

    const project = repository.loadProject();
    const rawEnvelopes = project.events.map((event) => ({
      id: event.id,
      projectId: event.projectId,
      sequence: event.sequence,
      sessionId: event.sessionId,
      type: event.type,
      schemaVersion: event.schemaVersion,
      timestamp: event.timestamp,
      correlationId: event.correlationId,
      causationId: event.causationId,
      payload: event.payload,
    }));

    // Artifact bytes: frontmatter ids normalized through the same map and
    // timestamps assigned from the sorted artifact order.
    const mappedArtifacts = [];
    let artifactIndex = 0;
    for (const kind of readdirSync(paths.artifactsDir).sort()) {
      const kindDir = join(paths.artifactsDir, kind);
      for (const file of readdirSync(kindDir).sort()) {
        const content = readFileSync(join(kindDir, file), 'utf8');
        const lines = content.split('\n');
        const closeIndex = lines.findIndex((line, index) => index > 0 && line.trim() === '---');
        const frontmatter = parseYaml(lines.slice(1, closeIndex).join('\n'));
        const body = lines.slice(closeIndex + 1).join('\n').replace(/^\n+/, '');
        const timestamp = normalizer.artifactTimestamp(artifactIndex);
        artifactIndex += 1;
        const mappedFrontmatter = {
          ...normalizer.walk(frontmatter),
          createdAt: timestamp,
          updatedAt: timestamp,
        };
        const rendered = `---\n${stringifyYaml(mappedFrontmatter).trimEnd()}\n---\n\n${normalizer.walk(body)}`;
        mappedArtifacts.push({ relativePath: `${kind}/${file}`, content: rendered });
      }
    }

    db.close();
    return {
      envelopes: rawEnvelopes.map((envelope) => normalizer.normalizeEvent(envelope)),
      artifacts: mappedArtifacts,
    };
  } finally {
    rmSync(workspace, { recursive: true, force: true });
    rmSync(isolatedHome, { recursive: true, force: true });
  }
}

/** Emit the end-to-end demo case: events, expected state/views, and artifact bytes. */
async function emitEndToEndCase(outDir) {
  const run = await runDemoWorkflow();
  const envelopes = withHashChain(run.envelopes, 1);
  const lines = envelopes.map((envelope) => toCanonicalLine(envelope));
  const eventsJsonl = `${lines.join('\n')}\n`;
  const violations = verifyCanonicalLog(eventsJsonl);
  if (violations.length > 0) {
    throw new Error(`end-to-end case produced a non-canonical log: ${violations.join('; ')}`);
  }

  const typed = envelopes.map((envelope) => validateStoredEvent(envelope));
  const state = replayProject(typed);
  const status = buildStatusView(state, typed, DEMO_SESSION_ID);
  const why = buildWhyView(state, typed);

  const files = new Map([
    ['events.jsonl', eventsJsonl],
    ['expected-state.json', canonicalDocument(state)],
    ['expected-status.json', canonicalDocument(status)],
    ['expected-why.json', canonicalDocument(why)],
  ]);
  for (const artifact of run.artifacts) {
    files.set(join('artifacts', artifact.relativePath), artifact.content);
  }
  files.set(
    'meta.json',
    canonicalDocument({
      formatVersion: FIXTURE_FORMAT_VERSION,
      name: 'e2e-demo-run',
      description:
        'A full seven-mode workflow run driven by the deterministic demo fake provider over the effective default configuration with the provider forced to fake; ids, timestamps, durations, and the workspace root are normalized per docs/event-format.md §11.',
      valid: true,
      envelopeVersion: 1,
      eventTypes: envelopes.map((envelope) => envelope.type),
      sessionKey: DEMO_SESSION_ID,
      normalization: '§11 (ids, timestamps, durationMs, workspace root)',
      artifacts: run.artifacts.map((artifact) => artifact.relativePath),
      generatedBy: GENERATED_BY,
      spec: SPEC,
    }),
  );
  writeCaseFiles(join(outDir, 'cases', 'e2e-demo-run'), files);
}

// ---------------------------------------------------------------------------
// Configuration corpus
// ---------------------------------------------------------------------------

const LEGACY_INLINE_YAML = `version: 1
budgets: {}
routing:
  discovery: { provider: anthropic, model: claude-sonnet-5-5 }
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: { provider: anthropic, model: claude-opus-5-5 }
  planning: { provider: openai, model: gpt-5-mini, fallbacks: [{ provider: anthropic, model: claude-sonnet-5-5 }] }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
`;

const LEGACY_LIMITS_YAML = `version: 1
budgets: {}
routing:
  discovery:
    provider: openai
    model: gpt-5.2
    fallbacks:
      - { provider: anthropic, model: claude-sonnet-5-5 }
    limits: { maxOutputTokens: 4096, temperature: 0.1 }
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: { provider: anthropic, model: claude-opus-5-5 }
  planning: { provider: anthropic, model: claude-sonnet-5-5 }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
`;

const ROLES_FULL_YAML = `version: 1
budgets: {}
models:
  worker:
    provider: fake
    model: fake-standard
  quick:
    provider: fake
    model: fake-mini
routing:
  intent: quick
  discovery: worker
  specification: worker
  technical-design: worker
  planning: worker
  implementation: worker
  verification: worker
  delivery: worker
`;

const ROLES_CHAIN_YAML = `version: 1
budgets: {}
models:
  a:
    provider: anthropic
    model: claude-sonnet-5-5
    fallbacks: [b]
    limits: { maxOutputTokens: 4096, temperature: 0.2 }
  b:
    provider: openai
    model: gpt-5.2
    limits: { maxOutputTokens: 2048 }
  reasoning:
    provider: anthropic
    model: claude-sonnet-5-5
routing:
  intent: reasoning
  discovery: a
  specification: reasoning
  technical-design: reasoning
  planning: reasoning
  implementation: reasoning
  verification: reasoning
  delivery: reasoning
`;

const MIXED_YAML = `version: 1
budgets: {}
models:
  heavy:
    provider: anthropic
    model: claude-opus-5-5
    fallbacks: [light]
  light:
    provider: anthropic
    model: claude-haiku-4-5
routing:
  intent: light
  discovery: light
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: heavy
  planning: { provider: openai, model: gpt-5.2, fallbacks: [{ provider: anthropic, model: claude-sonnet-5-5 }] }
  implementation: light
  verification: { provider: openai, model: gpt-5.2 }
  delivery: light
`;

/** Serialize a NormalizedRouting into a stable, cross-language comparable document. */
function serializeNormalizedRouting(routing) {
  const route = (entry) =>
    entry.limits === undefined
      ? { role: entry.role, provider: entry.provider, model: entry.model }
      : { role: entry.role, provider: entry.provider, model: entry.model, limits: entry.limits };
  const sortedEntries = (map) =>
    [...map.entries()]
      .sort((x, y) => (x[0] < y[0] ? -1 : x[0] > y[0] ? 1 : 0))
      .map(([key, chain]) => [key, chain.map(route)]);
  return {
    roleChains: sortedEntries(routing.roleChains),
    purposeChains: sortedEntries(routing.purposeChains),
    legacyFallback: routing.legacyFallback === null ? null : routing.legacyFallback.map(route),
  };
}

/** The config corpus: YAML input (or the built-in defaults) plus expected normalized output. */
const CONFIG_CASES = [
  {
    name: 'legacy-inline',
    description:
      'A complete version-1 legacy configuration: inline routes only, one with an inline fallback chain.',
    yaml: LEGACY_INLINE_YAML,
  },
  {
    name: 'legacy-inline-limits',
    description: 'Legacy inline routes carrying per-route limits and an inline fallback.',
    yaml: LEGACY_LIMITS_YAML,
  },
  {
    name: 'roles-default',
    description:
      'The built-in default configuration (no YAML input): named roles for all seven modes plus intent.',
    yaml: null,
  },
  {
    name: 'roles-full',
    description:
      'A full role-based configuration: worker and quick roles referenced from every routing key.',
    yaml: ROLES_FULL_YAML,
  },
  {
    name: 'roles-fallback-chain-limits',
    description: 'Role fallback chains with per-role limits expanded breadth-first into the purpose chain.',
    yaml: ROLES_CHAIN_YAML,
  },
  {
    name: 'mixed-roles-and-inline',
    description: 'A mixed configuration: role references and legacy inline routes (with fallbacks) side by side.',
    yaml: MIXED_YAML,
  },
];

function emitConfigCorpus(outDir) {
  for (const configCase of CONFIG_CASES) {
    const config =
      configCase.yaml === null
        ? DEFAULT_CONFIG
        : parseRelayConfig(`${configCase.name}.yaml`, parseYaml(configCase.yaml));
    const normalized = serializeNormalizedRouting(normalizeRouting(config));
    const dir = join(outDir, 'config', configCase.name);
    rmSync(dir, { recursive: true, force: true });
    mkdirSync(dir, { recursive: true });
    if (configCase.yaml !== null) {
      writeFileSync(join(dir, 'input.yaml'), configCase.yaml);
    }
    writeFileSync(join(dir, 'expected-normalized.json'), canonicalDocument(normalized));
    writeFileSync(
      join(dir, 'meta.json'),
      canonicalDocument({
        formatVersion: FIXTURE_FORMAT_VERSION,
        name: configCase.name,
        description: configCase.description,
        input: configCase.yaml === null ? 'defaults' : 'input.yaml',
        generatedBy: GENERATED_BY,
      }),
    );
  }
}

// ---------------------------------------------------------------------------
// CLI corpus
// ---------------------------------------------------------------------------

/** Spawn the CLI (tsx over src) the same way test/integration/cli.test.ts does. */
function runCli(args, cwd, home) {
  const require = createRequire(import.meta.url);
  const tsx = require.resolve('tsx');
  const result = spawnSync(process.execPath, ['--import', tsx, join(REPO_ROOT, 'src', 'cli.tsx'), ...args], {
    cwd,
    encoding: 'utf8',
    timeout: 60_000,
    env: { ...process.env, HOME: home, RELAY_FAKE: '1', NO_COLOR: '1' },
  });
  return { code: result.status, stdout: result.stdout ?? '', stderr: result.stderr ?? '' };
}

/** Capture help/version text and observed exit codes for representative invocations. */
function emitCliCorpus(outDir) {
  const home = mkdtempSync(join(tmpdir(), 'relay-fixture-cli-home-'));
  const work = mkdtempSync(join(tmpdir(), 'relay-fixture-cli-work-'));
  try {
    const dir = join(outDir, 'cli');
    rmSync(dir, { recursive: true, force: true });
    mkdirSync(dir, { recursive: true });

    const help = runCli(['--help'], work, home);
    const version = runCli(['--version'], work, home);
    if (help.code !== 0 || version.code !== 0) {
      throw new Error(
        `CLI help/version capture failed: help=${String(help.code)} version=${String(version.code)}`,
      );
    }
    writeFileSync(join(dir, 'help.txt'), help.stdout);
    writeFileSync(join(dir, 'version.txt'), version.stdout);

    const cases = [
      { argv: ['--help'], note: 'help exits 0' },
      { argv: ['--version'], note: 'version exits 0' },
      { argv: [], note: 'no command is a usage error' },
      { argv: ['bogus'], note: 'unknown command is a usage error' },
      { argv: ['-x'], note: 'short options are a usage error (only long options supported)' },
      { argv: ['status'], note: 'stateful command without an initialized project is a runtime failure' },
      { argv: ['why'], note: 'stateful command without an initialized project is a runtime failure' },
      { argv: ['inspect'], note: 'inspect without a target is a usage error' },
      { argv: ['config', 'bogus'], note: 'unknown config action is a usage error' },
      { argv: ['config', 'get'], note: 'config get without a key is a usage error' },
    ];
    const observed = cases.map((entry) => {
      const result = runCli(entry.argv, work, home);
      if (result.code === null) {
        throw new Error(`CLI invocation ${JSON.stringify(entry.argv)} timed out`);
      }
      return { argv: entry.argv, exitCode: result.code, note: entry.note };
    });
    writeFileSync(
      join(dir, 'exit-codes.json'),
      canonicalDocument({
        formatVersion: FIXTURE_FORMAT_VERSION,
        exitCodes: { success: 0, runtimeFailure: 1, usageOrConfigError: 2 },
        errorClasses: {
          ConfigValidationError: 2,
          UsageError: 2,
          UnknownModelRoleError: 2,
          ProjectNotFoundError: 1,
          NotInitializedError: 1,
          ProjectAlreadyInitializedError: 1,
          ProviderFailure: 1,
          RouteUnavailableError: 1,
          PolicyRejectionError: 1,
          unclassifiedError: 1,
        },
        cases: observed,
        source: 'src/cli/main.ts and src/cli/errors.ts',
        generatedBy: GENERATED_BY,
      }),
    );
  } finally {
    rmSync(home, { recursive: true, force: true });
    rmSync(work, { recursive: true, force: true });
  }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

function parseOutArg(argv) {
  const index = argv.indexOf('--out');
  if (index !== -1 && index + 1 < argv.length) return argv[index + 1];
  return DEFAULT_OUT;
}

async function main() {
  const outDir = parseOutArg(process.argv.slice(2));
  for (const section of ['cases', 'config', 'cli']) {
    rmSync(join(outDir, section), { recursive: true, force: true });
  }

  const caseMetas = [];
  for (const caseSpec of VALID_CASES) {
    caseMetas.push(emitHandBuiltCase(outDir, caseSpec, true));
  }
  await emitEndToEndCase(outDir);
  caseMetas.push({
    name: 'e2e-demo-run',
    valid: true,
    eventTypes: ['(see cases/e2e-demo-run/meta.json)'],
  });
  for (const caseSpec of INVALID_CASES) {
    caseMetas.push(emitHandBuiltCase(outDir, caseSpec, false));
  }

  emitConfigCorpus(outDir);
  emitCliCorpus(outDir);

  const covered = new Set();
  for (const caseSpec of VALID_CASES) {
    for (const spec of caseSpec.specs()) covered.add(spec.type);
  }
  const missing = RELAY_EVENT_TYPES.filter((type) => !covered.has(type));
  if (missing.length > 0) {
    throw new Error(`hand-built corpus does not cover event types: ${missing.join(', ')}`);
  }

  writeFileSync(
    join(outDir, 'manifest.json'),
    canonicalDocument({
      formatVersion: FIXTURE_FORMAT_VERSION,
      generatedBy: GENERATED_BY,
      spec: SPEC,
      deterministic: true,
      eventTypesCovered: [...RELAY_EVENT_TYPES],
      caseNames: caseMetas.map((meta) => meta.name),
      configCaseNames: CONFIG_CASES.map((entry) => entry.name),
      cliFiles: ['cli/help.txt', 'cli/version.txt', 'cli/exit-codes.json'],
    }),
  );
  const validCount = caseMetas.filter((meta) => meta.valid).length;
  console.log(
    `relay fixtures: wrote ${caseMetas.length} cases (${validCount} valid, ${caseMetas.length - validCount} invalid), ` +
      `${CONFIG_CASES.length} config cases, and the CLI corpus to ${relative(process.cwd(), outDir) || outDir}`,
  );
}

main().catch((error) => {
  console.error(`relay fixtures: ${error instanceof Error ? error.stack : String(error)}`);
  process.exit(1);
});
