/**
 * Workflow mode contracts.
 *
 * Modes are the seven stages a product idea travels through. Each mode
 * declares the artifact it produces, the inputs it requires, the tool
 * capabilities it permits, and whether it may modify user files. These
 * contracts drive both the transition policy and the permission policy.
 */

import { type ArtifactKind } from './artifacts.js';

export const WORKFLOW_MODES = [
  'discovery',
  'specification',
  'technical-design',
  'planning',
  'implementation',
  'verification',
  'delivery',
] as const;

export type WorkflowMode = (typeof WORKFLOW_MODES)[number];

export function isWorkflowMode(value: unknown): value is WorkflowMode {
  return typeof value === 'string' && (WORKFLOW_MODES as readonly string[]).includes(value);
}

/** Side-effect classes a tool can carry. Modes permit classes; policy gates execution. */
export type ToolCapability = 'read' | 'write' | 'execute';

/** Canonical mode order used for "earliest affected stage" routing. */
export function modeIndex(mode: WorkflowMode): number {
  return WORKFLOW_MODES.indexOf(mode);
}

export type ModeContract = {
  readonly mode: WorkflowMode;
  readonly title: string;
  readonly description: string;
  /** Artifact this mode is expected to produce. */
  readonly producedArtifact: ArtifactKind;
  /** Kinds that must exist before this mode can run. */
  readonly requiredInputs: readonly ArtifactKind[];
  /** Tool capabilities permitted while in this mode. */
  readonly permittedToolCapabilities: readonly ToolCapability[];
  /** Whether work in this mode may modify files in the user's workspace. */
  readonly canModifyUserFiles: boolean;
  /** Human-readable description of the evidence required to complete the mode. */
  readonly completionEvidence: string;
  readonly previousMode: WorkflowMode | null;
  readonly nextMode: WorkflowMode | null;
};

export const MODE_CONTRACTS: Readonly<Record<WorkflowMode, ModeContract>> = {
  discovery: {
    mode: 'discovery',
    title: 'Discovery',
    description: 'Capture the problem, users, evidence, outcomes, assumptions, and open questions.',
    producedArtifact: 'opportunity-brief',
    requiredInputs: [],
    permittedToolCapabilities: ['read'],
    canModifyUserFiles: false,
    completionEvidence: 'An opportunity brief covering problem, users, evidence, outcomes, assumptions, constraints, and open questions.',
    previousMode: null,
    nextMode: 'specification',
  },
  specification: {
    mode: 'specification',
    title: 'Specification',
    description: 'Turn the brief into testable requirements with explicit exclusions and acceptance criteria.',
    producedArtifact: 'prd',
    requiredInputs: ['opportunity-brief'],
    permittedToolCapabilities: ['read'],
    canModifyUserFiles: false,
    completionEvidence: 'A PRD with testable requirements, exclusions, and acceptance criteria, ready for approval as a contract.',
    previousMode: 'discovery',
    nextMode: 'technical-design',
  },
  'technical-design': {
    mode: 'technical-design',
    title: 'Technical design',
    description: 'Map requirements onto the target repository: architecture, interfaces, migrations, risks, rollout, rollback.',
    producedArtifact: 'trd',
    requiredInputs: ['prd'],
    permittedToolCapabilities: ['read', 'execute'],
    canModifyUserFiles: false,
    completionEvidence: 'A TRD grounded in repository facts with architecture, interfaces, migration notes, risks, and rollout/rollback.',
    previousMode: 'specification',
    nextMode: 'planning',
  },
  planning: {
    mode: 'planning',
    title: 'Planning',
    description: 'Produce an executable task graph with dependencies, affected files, checks, and integration points.',
    producedArtifact: 'execution-plan',
    requiredInputs: ['prd', 'trd'],
    permittedToolCapabilities: ['read', 'execute'],
    canModifyUserFiles: false,
    completionEvidence: 'An execution plan whose tasks have dependencies, affected files, verification commands, and integration points.',
    previousMode: 'technical-design',
    nextMode: 'implementation',
  },
  implementation: {
    mode: 'implementation',
    title: 'Implementation',
    description: 'Execute approved, in-scope tasks while preserving user changes and recording evidence.',
    producedArtifact: 'decision-log',
    requiredInputs: ['execution-plan'],
    permittedToolCapabilities: ['read', 'write', 'execute'],
    canModifyUserFiles: true,
    completionEvidence: 'All in-scope plan tasks executed with recorded file changes and check results, leaving user changes intact.',
    previousMode: 'planning',
    nextMode: 'verification',
  },
  verification: {
    mode: 'verification',
    title: 'Verification',
    description: 'Independently evaluate the resulting diff and evidence against every approved acceptance criterion.',
    producedArtifact: 'verification-report',
    requiredInputs: ['execution-plan'],
    permittedToolCapabilities: ['read', 'execute'],
    canModifyUserFiles: false,
    completionEvidence: 'A verification report that scores every approved acceptance criterion pass/fail with evidence from the diff and test runs.',
    previousMode: 'implementation',
    nextMode: 'delivery',
  },
  delivery: {
    mode: 'delivery',
    title: 'Delivery',
    description: 'Assemble the trace from problem to requirement to change to evidence, and prepare (but do not perform) delivery.',
    producedArtifact: 'delivery-report',
    requiredInputs: ['execution-plan', 'verification-report'],
    permittedToolCapabilities: ['read'],
    canModifyUserFiles: false,
    completionEvidence: 'A delivery report linking requirements to changes and verification evidence, held before any external delivery action.',
    previousMode: 'verification',
    nextMode: null,
  },
};

/**
 * Autonomy bounds. `off` pauses at every checkpoint; `next-checkpoint` runs
 * until the next artifact approval or intervention; `until:<mode>` runs until
 * the workflow enters that mode; `workflow` runs the whole workflow but still
 * pauses for consequential choices, approvals, destructive operations, and
 * delivery.
 */
export type AutonomyBound = 'off' | 'next-checkpoint' | 'workflow' | `until:${WorkflowMode}`;

export const NAMED_AUTONOMY_BOUNDS = ['off', 'next-checkpoint', 'workflow'] as const;

export function parseAutonomyBound(value: unknown): AutonomyBound | null {
  if (typeof value !== 'string') return null;
  if ((NAMED_AUTONOMY_BOUNDS as readonly string[]).includes(value)) {
    return value as 'off' | 'next-checkpoint' | 'workflow';
  }
  if (value.startsWith('until:')) {
    const mode = value.slice('until:'.length);
    return isWorkflowMode(mode) ? (`until:${mode}` as AutonomyBound) : null;
  }
  return null;
}

export function isAutonomyBound(value: unknown): value is AutonomyBound {
  return parseAutonomyBound(value) !== null;
}

/** Target mode for `until:` bounds, or null for named bounds. */
export function autonomyTarget(bound: AutonomyBound): WorkflowMode | null {
  return bound.startsWith('until:') ? (bound.slice('until:'.length) as WorkflowMode) : null;
}
