/**
 * SQLite-backed append-only event store implementing the core `EventRecorder`
 * contract.
 *
 * The store is the persistence half of Relay's event sourcing: it validates
 * every payload against the schema registered for its event type before
 * anything is written, assigns id/sequence/timestamp/correlation identity per
 * project, and persists the payload as canonical JSON. Historical rows are
 * immutable — enforced again at the database level by triggers. Reads validate
 * every row through `validateStoredEvent` so corrupt, foreign, or
 * newer-version data surfaces as actionable errors during replay instead of
 * silently corrupting project state.
 */

import type { DatabaseSync, StatementSync } from 'node:sqlite';
import {
  EVENT_PAYLOAD_VERSIONS,
  RELAY_EVENT_TYPES,
  eventPayloadSchemas,
  InvalidEventPayloadError,
  UnknownEventTypeError,
  validateStoredEvent,
} from '../core/events.js';
import type {
  AnyRelayEvent,
  EventAppendInput,
  EventRecorder,
  RelayEventType,
  StoredEventEnvelope,
} from '../core/events.js';
import { newId, nowIso } from '../core/ids.js';
import type { RelayDatabase } from './database.js';

const INSERT_EVENT_SQL = `INSERT INTO events (
  id, project_id, sequence, session_id, type, schema_version, timestamp, correlation_id, causation_id, payload
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?);`;

const EVENT_COLUMNS =
  'id, project_id, sequence, session_id, type, schema_version, timestamp, correlation_id, causation_id, payload';

type EventRow = {
  readonly id: string;
  readonly project_id: string;
  readonly sequence: number;
  readonly session_id: string | null;
  readonly type: string;
  readonly schema_version: number;
  readonly timestamp: string;
  readonly correlation_id: string;
  readonly causation_id: string | null;
  readonly payload: string;
};

export class SqliteEventStore implements EventRecorder {
  private readonly db: RelayDatabase;

  constructor(db: RelayDatabase) {
    this.db = db;
  }

  /** Validate and append a single event, assigning identity and sequence. */
  async append(input: EventAppendInput): Promise<AnyRelayEvent> {
    const events = await this.appendAll([input]);
    const first = events[0];
    if (first === undefined) {
      throw new Error('append: internal error: no event was produced.');
    }
    return first;
  }

  /**
   * Validate and append a batch of events in one transaction with contiguous
   * per-project sequences. If any member is invalid, nothing is written and
   * the sequence counters are left unchanged.
   */
  async appendAll(inputs: readonly EventAppendInput[]): Promise<AnyRelayEvent[]> {
    const raw = this.db.raw;
    raw.exec('BEGIN IMMEDIATE;');
    let events: AnyRelayEvent[];
    try {
      events = this.insertAll(raw, inputs);
      raw.exec('COMMIT;');
    } catch (error) {
      try {
        raw.exec('ROLLBACK;');
      } catch {
        // The original failure may have already closed the transaction.
      }
      throw error;
    }
    return events;
  }

  /**
   * Append inside a transaction the caller has already opened (and owns).
   *
   * @internal Lets the project repository commit derived-table rows and the
   * corresponding events atomically. Validation still happens before any
   * write, so an invalid member aborts the caller's transaction cleanly.
   */
  appendAllInOpenTransaction(inputs: readonly EventAppendInput[]): readonly AnyRelayEvent[] {
    return this.insertAll(this.db.raw, inputs);
  }

  /**
   * Read a project's events in sequence order, validating every row. Rows with
   * sequence greater than `afterSequence` are returned, at most `limit` of
   * them.
   */
  readEvents(options: {
    readonly projectId: string;
    readonly afterSequence?: number;
    readonly limit?: number;
  }): readonly AnyRelayEvent[] {
    const raw = this.db.raw;
    const after = options.afterSequence ?? 0;
    const rows =
      options.limit === undefined
        ? raw
            .prepare(
              `SELECT ${EVENT_COLUMNS} FROM events WHERE project_id = ? AND sequence > ? ORDER BY sequence ASC;`,
            )
            .all(options.projectId, after)
        : raw
            .prepare(
              `SELECT ${EVENT_COLUMNS} FROM events WHERE project_id = ? AND sequence > ? ORDER BY sequence ASC LIMIT ?;`,
            )
            .all(options.projectId, after, options.limit);
    return rows.map((row) => rowToEvent(row));
  }

  /** Read every event across all projects, ordered by project then sequence, validating every row. */
  readAllEvents(): readonly AnyRelayEvent[] {
    const rows = this.db.raw
      .prepare(`SELECT ${EVENT_COLUMNS} FROM events ORDER BY project_id ASC, sequence ASC;`)
      .all();
    return rows.map((row) => rowToEvent(row));
  }

  /** Highest sequence number used for `projectId`, or 0 when the project has no events. */
  lastSequence(projectId: string): number {
    return this.readLastSequence(this.db.raw, projectId);
  }

  /** Sequence probe used for both validation diagnostics and insertion. Overridable for tests. */
  protected readLastSequence(raw: DatabaseSync, projectId: string): number {
    const row = raw
      .prepare('SELECT COALESCE(MAX(sequence), 0) AS last FROM events WHERE project_id = ?;')
      .get(projectId) as { last: number | bigint } | undefined;
    const last = row?.last ?? 0;
    return typeof last === 'number' ? last : Number(last);
  }

  private insertAll(raw: DatabaseSync, inputs: readonly EventAppendInput[]): AnyRelayEvent[] {
    if (inputs.length === 0) return [];

    // Validate every payload before anything is written, so an invalid member
    // aborts the whole batch with the store untouched.
    const nextByProject = new Map<string, number>();
    const prepared: { readonly input: EventAppendInput; readonly payload: unknown; readonly sequence: number }[] = [];
    for (const input of inputs) {
      let next = nextByProject.get(input.projectId);
      if (next === undefined) {
        next = this.readLastSequence(raw, input.projectId) + 1;
      }
      prepared.push({ input, payload: this.validatePayload(input, next), sequence: next });
      nextByProject.set(input.projectId, next + 1);
    }

    let insert: StatementSync;
    try {
      insert = raw.prepare(INSERT_EVENT_SQL);
    } catch (error) {
      throw new Error(
        'The events table is missing. Run Relay database migrations (runMigrations) before using the event store.',
        { cause: error },
      );
    }

    const events: AnyRelayEvent[] = [];
    for (const { input, payload, sequence } of prepared) {
      const envelope: StoredEventEnvelope = {
        id: newId('evt'),
        projectId: input.projectId,
        sequence,
        sessionId: input.sessionId ?? null,
        type: input.type,
        schemaVersion: EVENT_PAYLOAD_VERSIONS[input.type],
        timestamp: input.timestamp ?? nowIso(),
        correlationId: input.correlationId ?? newId('cor'),
        causationId: input.causationId ?? null,
        payload,
      };
      try {
        insert.run(
          envelope.id,
          envelope.projectId,
          envelope.sequence,
          envelope.sessionId,
          envelope.type,
          envelope.schemaVersion,
          envelope.timestamp,
          envelope.correlationId,
          envelope.causationId,
          JSON.stringify(envelope.payload),
        );
      } catch (error) {
        if (isUniqueViolation(error)) {
          throw new Error(
            `A concurrent writer already appended sequence ${sequence} for project ${input.projectId}; retry the operation.`,
            { cause: error },
          );
        }
        throw error;
      }
      // The returned event must be exactly what a later read would validate.
      events.push(validateStoredEvent(envelope));
    }
    return events;
  }

  private validatePayload(input: EventAppendInput, sequence: number): unknown {
    if (!(RELAY_EVENT_TYPES as readonly string[]).includes(input.type)) {
      throw new UnknownEventTypeError(
        String(input.type),
        `refusing to append to project ${input.projectId}: no payload schema is registered for this event type.`,
      );
    }
    const schema = eventPayloadSchemas[input.type];
    const parsed = schema.safeParse(input.payload);
    if (!parsed.success) {
      throw new InvalidEventPayloadError(
        input.type,
        sequence,
        parsed.error.issues
          .map((issue) => `${issue.path.join('.') || '(root)'}: ${issue.message}`)
          .join('; '),
      );
    }
    return parsed.data;
  }
}

function rowToEvent(row: Record<string, unknown>): AnyRelayEvent {
  const typed = row as EventRow;
  let payload: unknown;
  try {
    payload = JSON.parse(typed.payload);
  } catch (error) {
    throw new InvalidEventPayloadError(
      typed.type,
      typed.sequence,
      `payload is not valid JSON (${error instanceof Error ? error.message : String(error)}); the stored row may be corrupted`,
    );
  }
  return validateStoredEvent({
    id: typed.id,
    projectId: typed.project_id,
    sequence: typed.sequence,
    sessionId: typed.session_id,
    type: typed.type as RelayEventType,
    schemaVersion: typed.schema_version,
    timestamp: typed.timestamp,
    correlationId: typed.correlation_id,
    causationId: typed.causation_id,
    payload,
  });
}

function isUniqueViolation(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  const code: unknown = (error as { code?: unknown }).code;
  return code === 'ERR_SQLITE_ERROR' && error.message.includes('UNIQUE constraint failed');
}
