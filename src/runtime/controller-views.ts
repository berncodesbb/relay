/**
 * Read-only status and explanation views over project state and the event log.
 *
 * `buildStatusView` answers "/status": goal, mode, progress, artifacts,
 * changed files, blockers, and the next action. `buildWhyView` answers
 * "/why": the latest outcome, recent and rejected transitions, routing
 * decisions folded from the event tail, staleness, and any pause reason.
 * Both are pure projections — no events are appended.
 */

import { ARTIFACT_KINDS, isContractualArtifactKind, type ArtifactKind } from '../core/artifacts.js';
import type { AnyRelayEvent } from '../core/events.js';
import { MODE_CONTRACTS, modeIndex, WORKFLOW_MODES, type WorkflowMode } from '../core/modes.js';
import type { ArtifactStatus, ProjectState } from '../core/project-state.js';

export type StatusView = {
  readonly goal: string;
  readonly mode: WorkflowMode;
  readonly autonomy: string;
  readonly paused: {
    readonly by: 'user' | 'policy';
    readonly reason: string;
    readonly question: string | null;
    readonly options: readonly string[];
  } | null;
  readonly progress: { readonly stage: number; readonly total: number };
  readonly artifacts: readonly {
    readonly artifactId: string;
    readonly kind: ArtifactKind;
    readonly version: number;
    readonly status: ArtifactStatus;
    readonly title: string;
  }[];
  readonly staleArtifacts: readonly string[];
  readonly changedFiles: readonly string[];
  readonly blockers: readonly string[];
  readonly nextAction: string;
  readonly stats: { readonly events: number; readonly modelCalls: number; readonly toolCalls: number };
};

export type WhyView = {
  readonly mode: WorkflowMode;
  readonly lastOutcome: {
    readonly outcome: string;
    readonly reason: string;
    readonly evidence: readonly string[];
    readonly at: string;
  } | null;
  readonly recentTransitions: readonly {
    readonly from: WorkflowMode | null;
    readonly to: WorkflowMode;
    readonly reason: string;
    readonly at: string;
    readonly source: string;
  }[];
  readonly rejectedTransitions: readonly {
    readonly from: WorkflowMode;
    readonly to: WorkflowMode;
    readonly reason: string;
    readonly at: string;
  }[];
  readonly routing: readonly {
    readonly provider: string;
    readonly model: string;
    readonly purpose: string;
    /** Role of the attempted route; null for legacy inline (role-less) events. */
    readonly role: string | null;
    readonly at: string;
    readonly ok: boolean;
  }[];
  readonly staleArtifacts: readonly string[];
  readonly pausedReason: string | null;
};

/** Number of events folded into the routing tail of the why view. */
const ROUTING_TAIL_EVENTS = 40;
const TRANSITION_TAIL = 10;
const BLOCKER_LIMIT = 8;

/** Files changed by tool activity in `sessionId`, in order, deduplicated. */
export function changedFilesForSession(
  events: readonly AnyRelayEvent[],
  sessionId: string | null,
): readonly string[] {
  const files: string[] = [];
  for (const event of events) {
    if (event.type !== 'tool.executed') continue;
    if (sessionId !== null && event.sessionId !== sessionId) continue;
    for (const file of event.payload.affectedFiles) {
      if (!files.includes(file)) files.push(file);
    }
  }
  return files;
}

function nextActionFor(state: ProjectState): string {
  if (state.paused !== null) {
    return state.paused.question ?? state.paused.reason;
  }
  const contract = MODE_CONTRACTS[state.currentMode];
  const producedId = state.artifactsByKind[contract.producedArtifact];
  const produced = producedId !== undefined ? state.artifacts[producedId] : undefined;
  if (
    produced !== undefined &&
    produced.status === 'draft' &&
    isContractualArtifactKind(contract.producedArtifact)
  ) {
    return `approve the ${contract.producedArtifact} with /approve`;
  }
  if (state.currentMode === 'delivery' && state.artifactsByKind['delivery-report'] !== undefined) {
    return 'workflow complete — delivery actions require your authorization';
  }
  return `run /auto or relay run to continue ${state.currentMode}`;
}

function blockersFor(state: ProjectState): readonly string[] {
  const blockers: string[] = [];
  for (const artifactId of state.staleArtifacts) {
    const artifact = state.artifacts[artifactId];
    blockers.push(
      `${artifact?.kind ?? artifactId} is stale — regenerate it from its updated upstream inputs`,
    );
    if (blockers.length >= BLOCKER_LIMIT) return blockers;
  }
  if (state.paused !== null && blockers.length < BLOCKER_LIMIT) {
    blockers.push(`paused (${state.paused.by}): ${state.paused.reason}`);
  }
  return blockers;
}

/** Latest artifact per kind, in canonical kind order. */
function latestArtifacts(state: ProjectState): StatusView['artifacts'] {
  const listed: {
    artifactId: string;
    kind: ArtifactKind;
    version: number;
    status: ArtifactStatus;
    title: string;
  }[] = [];
  for (const kind of ARTIFACT_KINDS) {
    const artifactId = state.artifactsByKind[kind];
    if (artifactId === undefined) continue;
    const artifact = state.artifacts[artifactId];
    if (artifact === undefined) continue;
    listed.push({
      artifactId: artifact.artifactId,
      kind: artifact.kind,
      version: artifact.currentVersion,
      status: artifact.status,
      title: artifact.title,
    });
  }
  return listed;
}

export function buildStatusView(
  state: ProjectState,
  events: readonly AnyRelayEvent[],
  sessionId: string | null,
): StatusView {
  return {
    goal: state.goal,
    mode: state.currentMode,
    autonomy: state.autonomy,
    paused:
      state.paused === null
        ? null
        : {
          by: state.paused.by,
          reason: state.paused.reason,
          question: state.paused.question,
          options: [...state.paused.options],
        },
    progress: { stage: modeIndex(state.currentMode) + 1, total: WORKFLOW_MODES.length },
    artifacts: latestArtifacts(state),
    staleArtifacts: [...state.staleArtifacts],
    changedFiles: changedFilesForSession(events, sessionId),
    blockers: blockersFor(state),
    nextAction: nextActionFor(state),
    stats: {
      events: state.stats.events,
      modelCalls: state.stats.modelCalls,
      toolCalls: state.stats.toolCalls,
    },
  };
}

/**
 * Fold model.requested/model.failed events from the tail into routing rows.
 * A failure with a role prefers the request row for the same role and only
 * then falls back to the legacy provider+model+purpose match, so role-less
 * events keep rendering exactly as before.
 */
function routingTail(events: readonly AnyRelayEvent[]): WhyView['routing'] {
  const routing: {
    provider: string;
    model: string;
    purpose: string;
    role: string | null;
    at: string;
    ok: boolean;
  }[] = [];
  for (const event of events.slice(-ROUTING_TAIL_EVENTS)) {
    if (event.type === 'model.requested') {
      routing.push({
        provider: event.payload.provider,
        model: event.payload.model,
        purpose: event.payload.purpose,
        role: event.payload.role ?? null,
        at: event.timestamp,
        ok: true,
      });
    } else if (event.type === 'model.failed') {
      const failureRole = event.payload.role ?? null;
      const matchRow = (requireRoleMatch: boolean): number => {
        for (let index = routing.length - 1; index >= 0; index -= 1) {
          const entry = routing[index];
          if (entry === undefined || !entry.ok) continue;
          if (entry.provider !== event.payload.provider) continue;
          if (entry.model !== event.payload.model) continue;
          if (entry.purpose !== event.payload.purpose) continue;
          if (requireRoleMatch && entry.role !== failureRole) continue;
          return index;
        }
        return -1;
      };
      let index = matchRow(failureRole !== null);
      if (index < 0 && failureRole !== null) index = matchRow(false);
      if (index >= 0) {
        const entry = routing[index];
        if (entry !== undefined) entry.ok = false;
      }
    }
  }
  return routing;
}

export function buildWhyView(state: ProjectState, events: readonly AnyRelayEvent[]): WhyView {
  return {
    mode: state.currentMode,
    lastOutcome:
      state.lastOutcome === null
        ? null
        : {
          outcome: state.lastOutcome.outcome,
          reason: state.lastOutcome.reason,
          evidence: [...state.lastOutcome.evidence],
          at: state.lastOutcome.at,
        },
    recentTransitions: state.transitions.slice(-TRANSITION_TAIL).map((transition) => ({
      from: transition.from,
      to: transition.to,
      reason: transition.reason,
      at: transition.at,
      source: transition.source,
    })),
    rejectedTransitions: state.rejectedTransitions.slice(-TRANSITION_TAIL).map((rejected) => ({
      from: rejected.from,
      to: rejected.to,
      reason: rejected.reason,
      at: rejected.at,
    })),
    routing: routingTail(events),
    staleArtifacts: [...state.staleArtifacts],
    pausedReason: state.paused === null ? null : state.paused.reason,
  };
}
