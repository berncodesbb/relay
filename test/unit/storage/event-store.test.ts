import type { DatabaseSync } from 'node:sqlite';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import {
  IncompatibleEventVersionError,
  InvalidEventPayloadError,
  UnknownEventTypeError,
  validateStoredEvent,
  type EventAppendInput,
} from '../../../src/core/events.js';
import { openDatabase, type RelayDatabase } from '../../../src/storage/database.js';
import { runMigrations } from '../../../src/storage/migrations.js';
import { SqliteEventStore } from '../../../src/storage/event-store.js';

let dir: string;
let db: RelayDatabase | undefined;
let store: SqliteEventStore;

beforeEach(() => {
  dir = mkdtempSync(join(tmpdir(), 'relay-storage-events-'));
  db = openDatabase(join(dir, 'state.db'));
  runMigrations(db);
  store = new SqliteEventStore(db);
});

afterEach(() => {
  db?.close();
  db = undefined;
  rmSync(dir, { recursive: true, force: true });
});

function sessionStarted(projectId: string, sessionId: string): EventAppendInput {
  return { type: 'session.started', projectId, payload: { sessionId, interactive: true } };
}

function insertRawEvent(row: {
  id: string;
  projectId: string;
  sequence: number;
  type: string;
  schemaVersion: number;
  payload: string;
}): void {
  db?.raw
    .prepare(
      `INSERT INTO events (
        id, project_id, sequence, session_id, type, schema_version, timestamp, correlation_id, causation_id, payload
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?);`,
    )
    .run(
      row.id,
      row.projectId,
      row.sequence,
      null,
      row.type,
      row.schemaVersion,
      '2026-10-12T00:00:00.000Z',
      `cor_${row.id}`,
      null,
      row.payload,
    );
}

describe('SqliteEventStore.append', () => {
  it('assigns sequential sequences per project, independent across projects', async () => {
    await store.append(sessionStarted('prj_a', 's1'));
    await store.append(sessionStarted('prj_b', 's2'));
    await store.append(sessionStarted('prj_a', 's3'));

    expect(store.lastSequence('prj_a')).toBe(2);
    expect(store.lastSequence('prj_b')).toBe(1);

    const a = store.readEvents({ projectId: 'prj_a' });
    const b = store.readEvents({ projectId: 'prj_b' });
    expect(a.map((event) => event.sequence)).toEqual([1, 2]);
    expect(b.map((event) => event.sequence)).toEqual([1]);
  });

  it('round-trips the returned event through validateStoredEvent with assigned identity', async () => {
    const event = await store.append({
      type: 'project.initialized',
      projectId: 'prj_a',
      payload: { goal: 'Ship it', workspaceRoot: '/w', initialAutonomy: 'workflow' },
    });
    expect(() => validateStoredEvent(event)).not.toThrow();
    expect(validateStoredEvent(event)).toEqual(event);

    expect(event.id).toMatch(/^evt_/);
    expect(event.correlationId).toMatch(/^cor_/);
    expect(event.causationId).toBeNull();
    expect(event.sessionId).toBeNull();
    expect(event.sequence).toBe(1);
    expect(event.timestamp).toMatch(/^\d{4}-\d{2}-\d{2}T/);
  });

  it('honors caller-supplied identity fields', async () => {
    const event = await store.append({
      type: 'session.started',
      projectId: 'prj_a',
      sessionId: 'ses_1',
      payload: { sessionId: 'ses_1', interactive: false },
      timestamp: '2026-10-12T01:02:03.000Z',
      correlationId: 'cor_given',
      causationId: 'evt_cause',
    });
    expect(event.sessionId).toBe('ses_1');
    expect(event.timestamp).toBe('2026-10-12T01:02:03.000Z');
    expect(event.correlationId).toBe('cor_given');
    expect(event.causationId).toBe('evt_cause');
  });

  it('rejects an invalid payload before writing anything', async () => {
    const append = store.append({
      type: 'project.initialized',
      projectId: 'prj_a',
      // goal missing on purpose
      payload: { workspaceRoot: '/w', initialAutonomy: 'workflow' } as never,
    });
    await expect(append).rejects.toBeInstanceOf(InvalidEventPayloadError);
    await expect(append).rejects.toThrow('sequence 1');

    expect(store.lastSequence('prj_a')).toBe(0);
    expect(store.readAllEvents()).toEqual([]);
  });

  it('rejects an unknown event type at runtime', async () => {
    const append = store.append({
      type: 'time.traveled',
      projectId: 'prj_a',
      payload: {},
    } as unknown as EventAppendInput);
    await expect(append).rejects.toBeInstanceOf(UnknownEventTypeError);
    expect(store.readAllEvents()).toEqual([]);
  });
});

describe('append-only enforcement', () => {
  it('rejects UPDATE and DELETE on the events table', async () => {
    await store.append(sessionStarted('prj_a', 's1'));

    expect(() => db?.raw.exec("UPDATE events SET type = 'session.started';")).toThrow(
      'events table is append-only',
    );
    expect(() => db?.raw.exec('DELETE FROM events;')).toThrow('events table is append-only');

    expect(store.readEvents({ projectId: 'prj_a' })).toHaveLength(1);
  });

  it('rejects duplicate (project, sequence) rows at the database level', async () => {
    await store.append(sessionStarted('prj_a', 's1'));
    expect(() =>
      insertRawEvent({
        id: 'evt_dup',
        projectId: 'prj_a',
        sequence: 1,
        type: 'session.started',
        schemaVersion: 1,
        payload: '{"sessionId":"s","interactive":true}',
      }),
    ).toThrow('UNIQUE constraint failed');
  });
});

describe('SqliteEventStore.appendAll', () => {
  it('writes a batch in one transaction with contiguous sequences', async () => {
    const events = await store.appendAll([
      sessionStarted('prj_a', 's1'),
      sessionStarted('prj_a', 's2'),
      sessionStarted('prj_a', 's3'),
    ]);
    expect(events.map((event) => event.sequence)).toEqual([1, 2, 3]);
    expect(store.lastSequence('prj_a')).toBe(3);
  });

  it('rolls the whole batch back when any member is invalid', async () => {
    const good = sessionStarted('prj_a', 's1');
    const bad: EventAppendInput = {
      type: 'session.started',
      projectId: 'prj_a',
      payload: { sessionId: '', interactive: true }, // sessionId must be non-empty
    };
    const append = store.appendAll([good, bad, sessionStarted('prj_a', 's3')]);
    await expect(append).rejects.toBeInstanceOf(InvalidEventPayloadError);

    expect(store.lastSequence('prj_a')).toBe(0);
    expect(store.readEvents({ projectId: 'prj_a' })).toEqual([]);
  });

  it('surfaces an actionable error when a concurrent writer took the sequence', async () => {
    const stale = new StaleSequenceStore(db as RelayDatabase);
    await stale.append(sessionStarted('prj_a', 's1'));

    const second = stale.append(sessionStarted('prj_a', 's2'));
    await expect(second).rejects.toThrow('concurrent writer already appended sequence 1');

    // The failed append left the log untouched.
    expect(store.readEvents({ projectId: 'prj_a' })).toHaveLength(1);
  });
});

describe('SqliteEventStore reads', () => {
  it('pages with afterSequence and limit', async () => {
    for (let i = 1; i <= 5; i += 1) {
      await store.append(sessionStarted('prj_a', `s${i}`));
    }
    expect(store.readEvents({ projectId: 'prj_a', limit: 2 }).map((e) => e.sequence)).toEqual([1, 2]);
    expect(
      store.readEvents({ projectId: 'prj_a', afterSequence: 2, limit: 2 }).map((e) => e.sequence),
    ).toEqual([3, 4]);
    expect(store.readEvents({ projectId: 'prj_a', afterSequence: 4 }).map((e) => e.sequence)).toEqual([5]);
  });

  it('orders readAllEvents by project then sequence', async () => {
    await store.append(sessionStarted('prj_b', 's1'));
    await store.append(sessionStarted('prj_a', 's1'));
    await store.append(sessionStarted('prj_b', 's2'));
    const keys = store.readAllEvents().map((event) => `${event.projectId}:${event.sequence}`);
    expect(keys).toEqual(['prj_a:1', 'prj_b:1', 'prj_b:2']);
  });

  it('surfaces UnknownEventTypeError for a row with a foreign event type', () => {
    insertRawEvent({
      id: 'evt_bad_type',
      projectId: 'prj_c',
      sequence: 1,
      type: 'time.traveled',
      schemaVersion: 1,
      payload: '{}',
    });
    expect(() => store.readEvents({ projectId: 'prj_c' })).toThrow(UnknownEventTypeError);
    expect(() => store.readEvents({ projectId: 'prj_c' })).toThrow(/newer Relay|different tool/);
  });

  it('surfaces IncompatibleEventVersionError for a newer schema version', () => {
    insertRawEvent({
      id: 'evt_bad_ver',
      projectId: 'prj_c',
      sequence: 2,
      type: 'session.started',
      schemaVersion: 99,
      payload: '{"sessionId":"s","interactive":true}',
    });
    expect(() => store.readEvents({ projectId: 'prj_c' })).toThrow(IncompatibleEventVersionError);
    expect(() => store.readEvents({ projectId: 'prj_c' })).toThrow(/version 99/);
  });

  it('surfaces InvalidEventPayloadError for a corrupted payload', () => {
    insertRawEvent({
      id: 'evt_bad_json',
      projectId: 'prj_c',
      sequence: 3,
      type: 'session.started',
      schemaVersion: 1,
      payload: '{not json',
    });
    expect(() => store.readEvents({ projectId: 'prj_c' })).toThrow(InvalidEventPayloadError);
    expect(() => store.readEvents({ projectId: 'prj_c' })).toThrow('not valid JSON');

    insertRawEvent({
      id: 'evt_bad_shape',
      projectId: 'prj_c',
      sequence: 4,
      type: 'session.started',
      schemaVersion: 1,
      payload: '{"interactive":"yes"}',
    });
    expect(() => store.readAllEvents()).toThrow(InvalidEventPayloadError);
  });
});

/** Simulates a store whose sequence probe races behind another writer. */
class StaleSequenceStore extends SqliteEventStore {
  protected override readLastSequence(_raw: DatabaseSync, _projectId: string): number {
    return 0;
  }
}
