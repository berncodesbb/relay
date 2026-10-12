/**
 * Project state contracts.
 *
 * `ProjectState` is the deterministic projection of the event log produced by
 * the pure reducer in `src/core/reducer.ts`. It never touches storage,
 * providers, or the terminal.
 */

import type { ArtifactKind } from './artifacts.js';
import type { AutonomyBound, WorkflowMode } from './modes.js';
import type { OutcomeKind } from './outcomes.js';

export type ArtifactStatus = 'draft' | 'approved' | 'stale';

export type ArtifactState = {
  readonly artifactId: string;
  readonly kind: ArtifactKind;
  readonly title: string;
  readonly currentVersion: number;
  readonly status: ArtifactStatus;
  /** Path relative to the workspace root, when the artifact has been rendered. */
  readonly path: string | null;
  readonly createdAt: string;
  readonly lastChangedAt: string;
  readonly approvedAt: string | null;
  readonly revisions: readonly {
    readonly version: number;
    readonly at: string;
    readonly reason: string;
  }[];
};

export type TransitionRecord = {
  readonly at: string;
  readonly from: WorkflowMode | null;
  readonly to: WorkflowMode;
  readonly reason: string;
  readonly source: 'auto' | 'user';
};

export type PauseState = {
  readonly by: 'user' | 'policy';
  readonly reason: string;
  readonly question: string | null;
  readonly options: readonly string[];
  readonly at: string;
};

export type OutcomeRecord = {
  readonly at: string;
  readonly outcome: OutcomeKind;
  readonly to: WorkflowMode | null;
  readonly reviseStage: WorkflowMode | null;
  readonly reason: string;
  readonly evidence: readonly string[];
};

export type VerificationSummary = {
  readonly passed: boolean | null;
  readonly lastRunAt: string | null;
  readonly passedCount: number;
  readonly failedCount: number;
  readonly unevaluatedCount: number;
};

export type ProjectState = {
  readonly projectId: string;
  readonly goal: string;
  readonly workspaceRoot: string | null;
  readonly initializedAt: string;
  readonly currentMode: WorkflowMode;
  readonly autonomy: AutonomyBound;
  readonly paused: PauseState | null;
  /** artifactId -> state, insertion order preserved. */
  readonly artifacts: Readonly<Record<string, ArtifactState>>;
  /** Latest artifact id per kind. */
  readonly artifactsByKind: Readonly<Partial<Record<ArtifactKind, string>>>;
  readonly transitions: readonly TransitionRecord[];
  readonly rejectedTransitions: readonly {
    readonly at: string;
    readonly from: WorkflowMode;
    readonly to: WorkflowMode;
    readonly reason: string;
  }[];
  readonly lastOutcome: OutcomeRecord | null;
  readonly stats: {
    readonly events: number;
    readonly modelCalls: number;
    readonly toolCalls: number;
    readonly lastModelError: string | null;
  };
  readonly staleArtifacts: readonly string[];
  readonly verification: VerificationSummary;
  readonly lastActivityAt: string;
};

export function createEmptyProjectState(): ProjectState {
  return {
    projectId: '',
    goal: '',
    workspaceRoot: null,
    initializedAt: '',
    currentMode: 'discovery',
    autonomy: 'next-checkpoint',
    paused: null,
    artifacts: {},
    artifactsByKind: {},
    transitions: [],
    rejectedTransitions: [],
    lastOutcome: null,
    stats: { events: 0, modelCalls: 0, toolCalls: 0, lastModelError: null },
    staleArtifacts: [],
    verification: { passed: null, lastRunAt: null, passedCount: 0, failedCount: 0, unevaluatedCount: 0 },
    lastActivityAt: '',
  };
}
