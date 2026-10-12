/**
 * Repository for Relay projects backed by the SQLite event store.
 *
 * The repository owns the `projects` listing table (a derived index — the
 * event log stays the source of truth), initializes projects with their first
 * `project.initialized` event in the same transaction as the index row, and
 * reconstructs state by folding a project's events through an injected
 * reducer. The fold is injected so this module never depends on the reducer
 * implementation, keeping storage independent of workflow logic.
 */

import type { AnyRelayEvent } from '../core/events.js';
import { newId, nowIso } from '../core/ids.js';
import type { ProjectState } from '../core/project-state.js';
import type { RelayDatabase } from './database.js';
import type { SqliteEventStore } from './event-store.js';

/** Folds a project's validated events (in sequence order) into state. */
export type StateFold = (events: readonly AnyRelayEvent[]) => ProjectState;

/** Raised when no project exists, or the requested project id is unknown. */
export class ProjectNotFoundError extends Error {
  constructor(
    readonly detail: string,
  ) {
    super(`No Relay project found: ${detail}`);
    this.name = 'ProjectNotFoundError';
  }
}

/** Listing entry for a project, backed by the derived `projects` table. */
export type ProjectSummary = {
  readonly projectId: string;
  readonly goal: string;
  readonly createdAt: string;
  readonly eventCount: number;
};

const INSERT_PROJECT_SQL = 'INSERT INTO projects (project_id, created_at, goal) VALUES (?, ?, ?);';

const LIST_PROJECTS_SQL = `SELECT
  p.project_id AS projectId,
  p.created_at AS createdAt,
  p.goal AS goal,
  (SELECT COUNT(*) FROM events e WHERE e.project_id = p.project_id) AS eventCount
FROM projects p
ORDER BY p.created_at ASC, p.project_id ASC;`;

export class ProjectRepository {
  private readonly db: RelayDatabase;
  private readonly store: SqliteEventStore;
  private readonly fold: StateFold;

  constructor(db: RelayDatabase, store: SqliteEventStore, fold: StateFold) {
    this.db = db;
    this.store = store;
    this.fold = fold;
  }

  /**
   * Create a new project: one transaction writes both the `projects` index row
   * and the initial `project.initialized` event, so the two can never diverge.
   * Returns the folded state of that single event.
   */
  async initializeProject(input: {
    readonly goal: string;
    readonly workspaceRoot: string;
    readonly autonomy: string;
  }): Promise<{ readonly projectId: string; readonly state: ProjectState }> {
    const projectId = newId('prj');
    const raw = this.db.raw;
    raw.exec('BEGIN IMMEDIATE;');
    let firstEvent: AnyRelayEvent | undefined;
    try {
      raw.prepare(INSERT_PROJECT_SQL).run(projectId, nowIso(), input.goal);
      const events = this.store.appendAllInOpenTransaction([
        {
          type: 'project.initialized',
          projectId,
          payload: {
            goal: input.goal,
            workspaceRoot: input.workspaceRoot,
            initialAutonomy: input.autonomy,
          },
        },
      ]);
      firstEvent = events[0];
      raw.exec('COMMIT;');
    } catch (error) {
      try {
        raw.exec('ROLLBACK;');
      } catch {
        // The original failure may have already closed the transaction.
      }
      throw error;
    }
    if (firstEvent === undefined) {
      throw new Error('Project initialization failed to produce an initial event.');
    }
    return { projectId, state: this.fold([firstEvent]) };
  }

  /**
   * Load a project by id, or the workspace's only project when no id is given.
   * Reconstructs state by folding the project's validated events. Throws
   * `ProjectNotFoundError` when there is no project or the id is unknown, and
   * an actionable error listing the available ids when several projects exist
   * and none was specified.
   */
  loadProject(projectId?: string): {
    readonly projectId: string;
    readonly state: ProjectState;
    readonly events: readonly AnyRelayEvent[];
  } {
    const resolved = projectId ?? this.singleProjectId();
    const events = this.store.readEvents({ projectId: resolved });
    if (events.length === 0) {
      throw new ProjectNotFoundError(
        `no events exist for project id "${resolved}". It may have been deleted, or the id is wrong. ` +
          'Run the status command to list known projects, or initialize a new project.',
      );
    }
    return { projectId: resolved, state: this.fold(events), events };
  }

  /** List all projects in the workspace with their event counts. */
  listProjects(): readonly ProjectSummary[] {
    const rows = this.db.raw.prepare(LIST_PROJECTS_SQL).all();
    return rows.map((row) => {
      const typed = row as { projectId: string; createdAt: string; goal: string; eventCount: number | bigint };
      return {
        projectId: typed.projectId,
        goal: typed.goal,
        createdAt: typed.createdAt,
        eventCount: Number(typed.eventCount),
      };
    });
  }

  private singleProjectId(): string {
    const projects = this.listProjects();
    if (projects.length === 0) {
      throw new ProjectNotFoundError(
        'this workspace has no Relay project yet. Initialize one before loading project state.',
      );
    }
    if (projects.length > 1) {
      const ids = projects.map((project) => project.projectId).join(', ');
      throw new Error(
        `This workspace has ${projects.length} Relay projects and no project id was given. ` +
          `Specify one of: ${ids}.`,
      );
    }
    const only = projects[0];
    if (only === undefined) {
      throw new Error('Project listing returned inconsistent results.');
    }
    return only.projectId;
  }
}
