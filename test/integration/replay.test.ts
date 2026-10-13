/**
 * Replay and restart integration: SQLite event store + project repository +
 * pure reducer, composed the way the CLI composes them. Verifies that Relay
 * reconstructs identical project state from persisted events across
 * restarts, survives interruption, replays incrementally, and surfaces
 * corruption with actionable diagnostics.
 */

import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { relayPaths } from '../../src/storage/paths.js';
import { openDatabase, type RelayDatabase } from '../../src/storage/database.js';
import { runMigrations } from '../../src/storage/migrations.js';
import { SqliteEventStore } from '../../src/storage/event-store.js';
import { ProjectRepository } from '../../src/storage/project-repository.js';
import { replayProject } from '../../src/core/reducer.js';
import type { AnyRelayEvent, EventAppendInput, RelayEventType } from '../../src/core/events.js';
import { buildWhyView } from '../../src/runtime/controller-views.js';
import { makeEvent, fullPipelineApproved, artifactRevised } from '../unit/core/event-factories.js';

const GOAL = 'Ship a workflow CLI that preserves product intent';

let workspace: string;
let db: RelayDatabase;
let store: SqliteEventStore;
let repository: ProjectRepository;
let projectId: string;

beforeEach(async () => {
  workspace = mkdtempSync(join(tmpdir(), 'relay-replay-'));
  db = openDatabase(relayPaths(workspace).database);
  runMigrations(db);
  store = new SqliteEventStore(db);
  repository = new ProjectRepository(db, store, replayProject);
  const initializedState = await repository.initializeProject({
    goal: GOAL,
    workspaceRoot: workspace,
    autonomy: 'workflow',
  });
  projectId = initializedState.projectId;
});

afterEach(() => {
  db.close();
  rmSync(workspace, { recursive: true, force: true });
});

function openFreshRepository(): ProjectRepository {
  // Simulates a process restart: brand-new handles over the same files.
  const fresh = openDatabase(relayPaths(workspace).database);
  runMigrations(fresh);
  return new ProjectRepository(fresh, new SqliteEventStore(fresh), replayProject);
}

async function appendAll(events: readonly AnyRelayEvent[]): Promise<void> {
  await store.appendAll(
    events.map(
      (event): EventAppendInput => ({
        type: event.type as RelayEventType,
        projectId,
        sessionId: event.sessionId,
        payload: event.payload as never,
        timestamp: event.timestamp,
      }),
    ),
  );
}

/** The pipeline events after `project.initialized` (which the repository owns). */
function pipelineAfterInit(): AnyRelayEvent[] {
  return fullPipelineApproved().slice(1);
}

describe('event-sourced project state', () => {
  it('reconstructs identical state after a full restart', async () => {
    await appendAll(pipelineAfterInit());

    const before = repository.loadProject();
    const after = openFreshRepository().loadProject();

    expect(after.projectId).toBe(before.projectId);
    expect(after.state).toEqual(before.state);
    expect(after.state.currentMode).toBe('delivery');
    expect(after.state.goal).toBe(GOAL);
    expect(Object.keys(after.state.artifacts)).toHaveLength(7);
    expect(after.state.verification.passed).toBe(true);
  });

  it('reconstructs state at an arbitrary interruption point', async () => {
    await appendAll(pipelineAfterInit().slice(0, 12)); // through plan approval + implementation entry

    const state = openFreshRepository().loadProject().state;
    expect(state.currentMode).toBe('implementation');
    expect(state.artifactsByKind['execution-plan']).toBeDefined();
    expect(state.paused).toBeNull();
  });

  it('folds an interruption event into a pause and resumes from persisted state', async () => {
    await appendAll(pipelineAfterInit().slice(0, 5));
    await appendAll([
      makeEvent({
        type: 'run.interrupted',
        projectId,
        payload: { phase: 'specification', hint: 'model stream canceled (Ctrl-C)' },
      }),
    ]);

    const interrupted = openFreshRepository().loadProject().state;
    expect(interrupted.paused?.by).toBe('user');
    expect(interrupted.paused?.reason).toContain('interrupted');

    await appendAll([
      makeEvent({
        type: 'workflow.resumed',
        projectId,
        payload: { by: 'user', note: 'resuming after review' },
      }),
    ]);
    const resumed = openFreshRepository().loadProject().state;
    expect(resumed.paused).toBeNull();
  });

  it('supports incremental replay from a persisted prefix plus tail', async () => {
    const events = pipelineAfterInit();
    await appendAll(events.slice(0, 7));
    const checkpoint = repository.loadProject();
    const lastSequence = checkpoint.events.at(-1)?.sequence ?? 0;

    await appendAll(events.slice(7));
    const tail = store.readEvents({ projectId, afterSequence: lastSequence });

    // Folding the checkpoint prefix + tail equals folding the entire log.
    const incremental = replayProject([...checkpoint.events, ...tail]);
    const full = replayProject(store.readEvents({ projectId }));
    expect(incremental).toEqual(full);
  });

  it('marks downstream artifacts stale when an approved PRD is revised, and keeps the chain durable', async () => {
    await appendAll(pipelineAfterInit());
    await appendAll([
      {
        ...artifactRevised('art-prd', 'prd', 1, 'user requested revision'),
        projectId,
      },
    ]);

    const state = openFreshRepository().loadProject().state;
    const statuses = Object.entries(state.artifacts).map(([id, a]) => `${id}:${a.status}`);
    expect(statuses).toContain('art-trd:stale');
    expect(statuses).toContain('art-plan:stale');
    expect(statuses).toContain('art-vr:stale');
    expect(statuses).toContain('art-dr:stale');
    expect(state.artifacts['art-prd']?.status).toBe('draft');
    expect(state.artifacts['art-brief']?.status).toBe('approved');
    expect(state.staleArtifacts).toEqual(
      expect.arrayContaining(['art-trd', 'art-plan', 'art-vr', 'art-dr']),
    );
  });

  it('replays and renders pre-role model events (role-less payloads stay valid)', async () => {
    await appendAll([
      makeEvent({
        type: 'model.requested',
        projectId,
        payload: { provider: 'anthropic', model: 'claude-sonnet-5-5', purpose: 'mode:discovery', mode: 'discovery', attempt: 1 },
      }),
      makeEvent({
        type: 'model.responded',
        projectId,
        payload: {
          provider: 'anthropic',
          model: 'claude-sonnet-5-5',
          purpose: 'mode:discovery',
          usage: { inputTokens: 10, outputTokens: 5 },
          toolCalls: 0,
          durationMs: 25,
        },
      }),
      makeEvent({
        type: 'model.failed',
        projectId,
        payload: { provider: 'anthropic', model: 'claude-sonnet-5-5', purpose: 'mode:discovery', errorKind: 'rate-limit', message: 'too many requests', attempts: 3, durationMs: 900 },
      }),
    ]);

    // Fresh handles: the log replays with identical state...
    const loaded = openFreshRepository().loadProject();
    expect(loaded.state.stats.modelCalls).toBe(1);
    expect(loaded.state.stats.lastModelError).toContain('rate-limit');

    // ...and the why view renders routing rows from role-less events: one row
    // per request, with the failure matched back onto its request row.
    const why = buildWhyView(loaded.state, loaded.events);
    expect(why.routing).toHaveLength(1);
    expect(why.routing[0]).toMatchObject({ provider: 'anthropic', model: 'claude-sonnet-5-5', purpose: 'mode:discovery' });
    expect(why.routing[0]?.ok).toBe(false);
  });

  it('rejects a sequence gap with an actionable diagnostic', async () => {
    await appendAll(pipelineAfterInit().slice(0, 4));
    // Sneak in a row that skips a sequence, bypassing the store's counter.
    db.raw
      .prepare(
        `INSERT INTO events (id, project_id, sequence, session_id, type, schema_version, timestamp, correlation_id, causation_id, payload)
         VALUES (?, ?, ?, NULL, ?, ?, ?, ?, NULL, ?)`,
      )
      .run(
        'evt_gap',
        projectId,
        9,
        'mode.entered',
        1,
        new Date().toISOString(),
        'corr_gap',
        JSON.stringify({ from: 'discovery', to: 'specification', reason: 'gap', source: 'auto' }),
      );

    expect(() => replayProject(store.readEvents({ projectId }))).toThrow(/sequence/i);
  });

  it('surfaces a corrupted payload with the event type and sequence', async () => {
    await appendAll(pipelineAfterInit().slice(0, 2));
    db.raw
      .prepare(
        `INSERT INTO events (id, project_id, sequence, session_id, type, schema_version, timestamp, correlation_id, causation_id, payload)
         VALUES (?, ?, ?, NULL, ?, ?, ?, ?, NULL, ?)`,
      )
      .run(
        'evt_bad',
        projectId,
        99,
        'artifact.approved',
        1,
        new Date().toISOString(),
        'corr_bad',
        JSON.stringify({ nonsense: true }),
      );

    expect(() => store.readEvents({ projectId })).toThrow(/artifact\.approved|sequence 99|invalid payload/s);
  });
});
