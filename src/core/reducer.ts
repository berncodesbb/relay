/**
 * Pure event fold for Relay project state.
 *
 * `applyEvent` folds one validated event onto a {@link ProjectState} and
 * `replayProject` folds a whole append-only log. Both are pure: no clocks,
 * randomness, or IO. Every timestamp comes from the event itself, so the same
 * log always yields identical state. Integrity violations (duplicate
 * initialization, broken revision chains, out-of-order sequences, mixed
 * projects) throw {@link EventFoldError} with actionable diagnostics.
 */

import { downstreamArtifactKinds } from './artifacts.js';
import {
  EVENT_PAYLOAD_VERSIONS,
  IncompatibleEventVersionError,
  UnknownEventTypeError,
  type AnyRelayEvent,
} from './events.js';
import { parseAutonomyBound } from './modes.js';
import { createEmptyProjectState, type ArtifactState, type ProjectState } from './project-state.js';

/** Raised when an event or event log cannot be folded into consistent state. */
export class EventFoldError extends Error {
  constructor(detail: string) {
    super(detail);
    this.name = 'EventFoldError';
  }
}

/** Cap on remembered rejected transitions; oldest are dropped beyond this. */
const MAX_REJECTED_TRANSITIONS = 50;

function requireArtifact(state: ProjectState, event: AnyRelayEvent, artifactId: string): ArtifactState {
  const artifact = state.artifacts[artifactId];
  if (!artifact) {
    throw new EventFoldError(
      `event "${event.type}" (sequence ${event.sequence}) references unknown artifact "${artifactId}"; ` +
        'the event log may be corrupted or was written by a different project',
    );
  }
  return artifact;
}

export function applyEvent(state: ProjectState, event: AnyRelayEvent): ProjectState {
  const knownVersion = (EVENT_PAYLOAD_VERSIONS as Readonly<Record<string, number | undefined>>)[event.type];
  if (knownVersion === undefined) {
    throw new UnknownEventTypeError(
      event.type,
      `at sequence ${event.sequence}: this reducer cannot fold the event; ` +
        'the log may come from a different tool or a newer Relay build',
    );
  }
  if (event.schemaVersion !== knownVersion) {
    throw new IncompatibleEventVersionError(event.type, event.schemaVersion, knownVersion);
  }

  // Common tail for every event: bump the event counter and record activity.
  const base: ProjectState = {
    ...state,
    stats: { ...state.stats, events: state.stats.events + 1 },
    lastActivityAt: event.timestamp,
  };

  switch (event.type) {
    case 'project.initialized': {
      if (state.goal !== '') {
        throw new EventFoldError(
          `duplicate project.initialized at sequence ${event.sequence}: project "${event.projectId}" ` +
            `was already initialized at ${state.initializedAt}; start a new project instead of reinitializing`,
        );
      }
      return {
        ...base,
        projectId: event.projectId,
        goal: event.payload.goal,
        workspaceRoot: event.payload.workspaceRoot,
        initializedAt: event.timestamp,
        autonomy: parseAutonomyBound(event.payload.initialAutonomy) ?? 'next-checkpoint',
        currentMode: 'discovery',
      };
    }

    case 'mode.entered': {
      const paused = state.paused !== null && state.paused.by === 'policy' ? null : state.paused;
      return {
        ...base,
        currentMode: event.payload.to,
        paused,
        transitions: [
          ...state.transitions,
          {
            at: event.timestamp,
            from: event.payload.from,
            to: event.payload.to,
            reason: event.payload.reason,
            source: event.payload.source,
          },
        ],
      };
    }

    case 'mode.transition.rejected': {
      const rejected = [
        ...state.rejectedTransitions,
        {
          at: event.timestamp,
          from: event.payload.from,
          to: event.payload.to,
          reason: event.payload.reason,
        },
      ];
      return {
        ...base,
        rejectedTransitions:
          rejected.length > MAX_REJECTED_TRANSITIONS ? rejected.slice(rejected.length - MAX_REJECTED_TRANSITIONS) : rejected,
      };
    }

    case 'autonomy.changed': {
      return { ...base, autonomy: parseAutonomyBound(event.payload.to) ?? 'next-checkpoint' };
    }

    case 'artifact.created': {
      if (state.artifacts[event.payload.artifactId] !== undefined) {
        throw new EventFoldError(
          `artifact.created at sequence ${event.sequence} reuses existing artifact id "${event.payload.artifactId}"; ` +
            'artifact ids are immutable — create a new artifact or revise the existing one',
        );
      }
      const artifact: ArtifactState = {
        artifactId: event.payload.artifactId,
        kind: event.payload.kind,
        title: event.payload.title,
        currentVersion: 1,
        status: 'draft',
        path: event.payload.path,
        createdAt: event.timestamp,
        lastChangedAt: event.timestamp,
        approvedAt: null,
        revisions: [],
      };
      return {
        ...base,
        artifacts: { ...base.artifacts, [event.payload.artifactId]: artifact },
        artifactsByKind: { ...base.artifactsByKind, [event.payload.kind]: event.payload.artifactId },
      };
    }

    case 'artifact.revised': {
      const existing = requireArtifact(base, event, event.payload.artifactId);
      if (existing.kind !== event.payload.kind) {
        throw new EventFoldError(
          `artifact.revised at sequence ${event.sequence} labels artifact "${event.payload.artifactId}" as ` +
            `"${event.payload.kind}" but it is a "${existing.kind}"; the event log may be corrupted`,
        );
      }
      if (event.payload.toVersion !== existing.currentVersion + 1) {
        throw new EventFoldError(
          `artifact.revised at sequence ${event.sequence} moves "${event.payload.artifactId}" (${event.payload.kind}) ` +
            `to version ${event.payload.toVersion}, but the current version is ${existing.currentVersion}; ` +
            `revisions must increment by exactly 1 (expected version ${existing.currentVersion + 1})`,
        );
      }
      const revised: ArtifactState = {
        ...existing,
        currentVersion: event.payload.toVersion,
        status: 'draft',
        approvedAt: null,
        path: event.payload.path,
        lastChangedAt: event.timestamp,
        revisions: [
          ...existing.revisions,
          { version: event.payload.toVersion, at: event.timestamp, reason: event.payload.reason },
        ],
      };
      // Deterministic downstream invalidation: every other artifact whose kind
      // is transitively downstream of the revised kind goes stale.
      const downstream = new Set<string>(downstreamArtifactKinds(event.payload.kind));
      const artifacts: Record<string, ArtifactState> = { ...base.artifacts, [event.payload.artifactId]: revised };
      const newlyStale: string[] = [];
      for (const [id, artifact] of Object.entries(base.artifacts)) {
        if (id === event.payload.artifactId || artifact.status === 'stale') continue;
        if (!downstream.has(artifact.kind)) continue;
        artifacts[id] = { ...artifact, status: 'stale' };
        newlyStale.push(id);
      }
      const staleArtifacts = base.staleArtifacts.filter((id) => id !== event.payload.artifactId);
      for (const id of newlyStale) {
        if (!staleArtifacts.includes(id)) staleArtifacts.push(id);
      }
      return { ...base, artifacts, staleArtifacts };
    }

    case 'artifact.invalidated': {
      const artifacts: Record<string, ArtifactState> = { ...base.artifacts };
      const staleArtifacts = [...base.staleArtifacts];
      for (const id of event.payload.artifactIds) {
        const artifact = artifacts[id];
        if (!artifact || artifact.status === 'stale') continue;
        artifacts[id] = { ...artifact, status: 'stale' };
        if (!staleArtifacts.includes(id)) staleArtifacts.push(id);
      }
      return { ...base, artifacts, staleArtifacts };
    }

    case 'artifact.approved': {
      const existing = requireArtifact(base, event, event.payload.artifactId);
      if (existing.kind !== event.payload.kind) {
        throw new EventFoldError(
          `artifact.approved at sequence ${event.sequence} labels artifact "${event.payload.artifactId}" as ` +
            `"${event.payload.kind}" but it is a "${existing.kind}"; the event log may be corrupted`,
        );
      }
      if (event.payload.version !== existing.currentVersion) {
        throw new EventFoldError(
          `artifact.approved at sequence ${event.sequence} targets ${event.payload.kind} ` +
            `"${event.payload.artifactId}" at version ${event.payload.version}, but the current version is ` +
            `${existing.currentVersion}; approve the current version after any revision`,
        );
      }
      return {
        ...base,
        artifacts: {
          ...base.artifacts,
          [event.payload.artifactId]: { ...existing, status: 'approved', approvedAt: event.timestamp },
        },
        staleArtifacts: base.staleArtifacts.filter((id) => id !== event.payload.artifactId),
      };
    }

    case 'transition.evaluated': {
      return {
        ...base,
        lastOutcome: {
          at: event.timestamp,
          outcome: event.payload.outcome,
          to: event.payload.to,
          reviseStage: event.payload.reviseStage,
          reason: event.payload.reason,
          evidence: [...event.payload.evidence],
        },
      };
    }

    case 'model.responded': {
      return { ...base, stats: { ...base.stats, modelCalls: base.stats.modelCalls + 1 } };
    }

    case 'model.failed': {
      return {
        ...base,
        stats: { ...base.stats, lastModelError: `${event.payload.errorKind}: ${event.payload.message}` },
      };
    }

    case 'tool.executed': {
      return { ...base, stats: { ...base.stats, toolCalls: base.stats.toolCalls + 1 } };
    }

    case 'verification.completed': {
      const results = event.payload.results;
      return {
        ...base,
        verification: {
          passed: event.payload.passed,
          lastRunAt: event.timestamp,
          passedCount: results.filter((result) => result.status === 'pass').length,
          failedCount: results.filter((result) => result.status === 'fail').length,
          unevaluatedCount: results.filter((result) => result.status === 'unevaluated').length,
        },
      };
    }

    case 'workflow.paused': {
      return {
        ...base,
        paused: {
          by: event.payload.by,
          reason: event.payload.reason,
          question: event.payload.question,
          options: [...event.payload.options],
          at: event.timestamp,
        },
      };
    }

    case 'workflow.resumed': {
      return { ...base, paused: null };
    }

    case 'budget.exhausted': {
      if (base.paused !== null) return base;
      return {
        ...base,
        paused: {
          by: 'policy',
          reason: `budget exhausted: ${event.payload.budget} (limit ${event.payload.limit})`,
          question: null,
          options: [],
          at: event.timestamp,
        },
      };
    }

    case 'run.interrupted': {
      if (base.paused !== null) return base;
      return {
        ...base,
        paused: {
          by: 'user',
          reason: `interrupted: ${event.payload.hint}`,
          question: null,
          options: [],
          at: event.timestamp,
        },
      };
    }

    // Activity-only events: the common tail already recorded them.
    case 'session.started':
    case 'intent.proposed':
    case 'intent.applied':
    case 'artifact.approval.requested':
    case 'artifact.inspected':
    case 'model.requested':
    case 'tool.denied':
      return base;

    default: {
      // Exhaustiveness guard: a union member without an explicit fold, or a
      // foreign object cast into AnyRelayEvent, must fail loudly.
      const unhandled = event as { readonly type: string; readonly sequence: number };
      throw new UnknownEventTypeError(
        unhandled.type,
        `at sequence ${unhandled.sequence}: the reducer has no fold for this event; ` +
          'the log may come from a different tool or a newer Relay build',
      );
    }
  }
}

export function replayProject(events: readonly AnyRelayEvent[]): ProjectState {
  const first = events[0];
  if (!first) {
    throw new EventFoldError(
      'cannot replay an empty event log: expected the first event to be project.initialized; ' +
        'the .relay project directory may be missing its event store',
    );
  }
  if (first.type !== 'project.initialized') {
    throw new EventFoldError(
      `event log must start with project.initialized, but the first event (sequence ${first.sequence}) is ` +
        `"${first.type}"; the log is truncated or corrupted — restore the .relay directory from a backup`,
    );
  }

  let state = createEmptyProjectState();
  const projectId = first.projectId;
  let expectedSequence = 1;
  for (const event of events) {
    if (event.projectId !== projectId) {
      throw new EventFoldError(
        `event "${event.type}" (sequence ${event.sequence}) belongs to project "${event.projectId}", ` +
          `but this log belongs to project "${projectId}"; each project keeps its own event log`,
      );
    }
    if (event.sequence !== expectedSequence) {
      throw new EventFoldError(
        `event log sequence mismatch at "${event.type}": found sequence ${event.sequence} but expected ` +
          `${expectedSequence} (sequences must increase by exactly 1 starting at 1); ` +
          'the log is corrupted, truncated, or contains duplicates',
      );
    }
    expectedSequence += 1;
    state = applyEvent(state, event);
  }
  return state;
}
