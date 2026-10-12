import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import type { AnyRelayEvent } from '../../../src/core/events.js';
import { parseAutonomyBound } from '../../../src/core/modes.js';
import { createEmptyProjectState, type ProjectState } from '../../../src/core/project-state.js';
import { openDatabase, type RelayDatabase } from '../../../src/storage/database.js';
import { runMigrations } from '../../../src/storage/migrations.js';
import { SqliteEventStore } from '../../../src/storage/event-store.js';
import {
  ProjectNotFoundError,
  ProjectRepository,
  type StateFold,
} from '../../../src/storage/project-repository.js';

/** Tiny inline projection: counts events and reads the init payload. */
const testFold: StateFold = (events) => {
  const initialized = events.find(
    (event): event is Extract<AnyRelayEvent, { type: 'project.initialized' }> =>
      event.type === 'project.initialized',
  );
  const last = events.at(-1);
  const state: ProjectState = {
    ...createEmptyProjectState(),
    projectId: initialized?.projectId ?? '',
    goal: initialized?.payload.goal ?? '',
    workspaceRoot: initialized?.payload.workspaceRoot ?? null,
    initializedAt: initialized?.timestamp ?? '',
    autonomy: parseAutonomyBound(initialized?.payload.initialAutonomy) ?? 'next-checkpoint',
    stats: { events: events.length, modelCalls: 0, toolCalls: 0, lastModelError: null },
    lastActivityAt: last?.timestamp ?? '',
  };
  return state;
};

let dir: string;
let dbPath: string;
let db: RelayDatabase | undefined;
let store: SqliteEventStore;
let repository: ProjectRepository;

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), 'relay-storage-repo-'));
  dbPath = join(dir, 'state.db');
  db = openDatabase(dbPath);
  runMigrations(db);
  store = new SqliteEventStore(db);
  repository = new ProjectRepository(db, store, testFold);
});

afterEach(() => {
  db?.close();
  db = undefined;
  rmSync(dir, { recursive: true, force: true });
});

describe('ProjectRepository.initializeProject', () => {
  it('creates the project and returns the folded state of its single event', async () => {
    const { projectId, state } = await repository.initializeProject({
      goal: 'Build the harness',
      workspaceRoot: '/w/repo',
      autonomy: 'until:planning',
    });

    expect(projectId).toMatch(/^prj_/);
    expect(state.projectId).toBe(projectId);
    expect(state.goal).toBe('Build the harness');
    expect(state.workspaceRoot).toBe('/w/repo');
    expect(state.autonomy).toBe('until:planning');
    expect(state.stats.events).toBe(1);
  });

  it('rolls the projects index row back when the initial event is invalid', async () => {
    const init = repository.initializeProject({
      goal: '', // goal must be non-empty
      workspaceRoot: '/w/repo',
      autonomy: 'workflow',
    });
    await expect(init).rejects.toThrow();
    expect(repository.listProjects()).toEqual([]);
    expect(store.readAllEvents()).toEqual([]);
  });
});

describe('ProjectRepository.loadProject', () => {
  it('round-trips a project through events and fold', async () => {
    const { projectId } = await repository.initializeProject({
      goal: 'Round trip',
      workspaceRoot: '/w/repo',
      autonomy: 'workflow',
    });

    await store.append({
      type: 'session.started',
      projectId,
      payload: { sessionId: 'ses_1', interactive: true },
    });

    const loaded = repository.loadProject();
    expect(loaded.projectId).toBe(projectId);
    expect(loaded.state.goal).toBe('Round trip');
    expect(loaded.state.stats.events).toBe(2);
    expect(loaded.events.map((event) => event.sequence)).toEqual([1, 2]);
  });

  it('reconstructs identical state after closing and reopening the database', async () => {
    const { projectId, state } = await repository.initializeProject({
      goal: 'Survive restarts',
      workspaceRoot: '/w/repo',
      autonomy: 'workflow',
    });
    db?.close();

    db = openDatabase(dbPath);
    runMigrations(db); // no-op on an already migrated database
    const reopenedStore = new SqliteEventStore(db);
    const reopenedRepository = new ProjectRepository(db, reopenedStore, testFold);

    const loaded = reopenedRepository.loadProject(projectId);
    expect(loaded.state).toEqual(state);
  });

  it('throws ProjectNotFoundError when the workspace has no projects', () => {
    expect(() => repository.loadProject()).toThrow(ProjectNotFoundError);
  });

  it('throws ProjectNotFoundError for an unknown project id', async () => {
    await repository.initializeProject({
      goal: 'Only project',
      workspaceRoot: '/w/repo',
      autonomy: 'workflow',
    });
    expect(() => repository.loadProject('prj_missing')).toThrow(ProjectNotFoundError);
  });

  it('refuses to pick a project when several exist and none is specified', async () => {
    const first = await repository.initializeProject({
      goal: 'First',
      workspaceRoot: '/w/a',
      autonomy: 'workflow',
    });
    const second = await repository.initializeProject({
      goal: 'Second',
      workspaceRoot: '/w/b',
      autonomy: 'workflow',
    });

    expect(() => repository.loadProject()).toThrow(
      expect.objectContaining({
        message: expect.stringMatching(new RegExp(`(${first.projectId}|${second.projectId}).*(${first.projectId}|${second.projectId})`)),
      }),
    );
    // Both ids must be listed, in whichever deterministic order the store uses.
    try {
      repository.loadProject();
    } catch (error) {
      const message = (error as Error).message;
      expect(message).toContain(first.projectId);
      expect(message).toContain(second.projectId);
    }
    expect(repository.loadProject(second.projectId).state.goal).toBe('Second');
  });
});

describe('ProjectRepository.listProjects', () => {
  it('lists projects with goals, creation timestamps, and event counts', async () => {
    const a = await repository.initializeProject({
      goal: 'Goal A',
      workspaceRoot: '/w/a',
      autonomy: 'workflow',
    });
    const b = await repository.initializeProject({
      goal: 'Goal B',
      workspaceRoot: '/w/b',
      autonomy: 'off',
    });
    await store.append({
      type: 'session.started',
      projectId: a.projectId,
      payload: { sessionId: 'ses_1', interactive: true },
    });

    const listed = repository.listProjects();
    expect(listed).toHaveLength(2);
    const byGoal = new Map(listed.map((project) => [project.goal, project]));
    expect(byGoal.get('Goal A')).toMatchObject({ projectId: a.projectId, eventCount: 2 });
    expect(byGoal.get('Goal B')).toMatchObject({ projectId: b.projectId, eventCount: 1 });
    for (const project of listed) {
      expect(project.createdAt).toMatch(/^\d{4}-\d{2}-\d{2}T/);
    }
  });
});
