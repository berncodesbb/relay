/**
 * Deterministic workflow transition policy.
 *
 * Given a {@link ProjectState} and a trigger, `evaluateTransition` produces the
 * single {@link ControllerOutcome} the controller must act on. Models may
 * propose transitions; only this policy emits outcomes. `canEnterMode` answers
 * whether a mode may be entered and why not, `earliestAffectedStage` routes
 * revision loops back to the earliest stage invalidated by an upstream
 * revision, and `shouldAutoAdvance` bounds auto-advancement by autonomy.
 */

import { downstreamArtifactKinds, isContractualArtifactKind, type ArtifactKind } from './artifacts.js';
import { autonomyTarget, MODE_CONTRACTS, modeIndex, WORKFLOW_MODES, type WorkflowMode } from './modes.js';
import type { ControllerOutcome } from './outcomes.js';
import type { ProjectState } from './project-state.js';

export type TransitionTrigger =
  | { readonly kind: 'mode-complete'; readonly evidence: readonly string[] }
  | { readonly kind: 'artifact-approved'; readonly artifactId: string }
  | { readonly kind: 'verification-failed'; readonly earliestAffectedStage: WorkflowMode }
  | { readonly kind: 'user-request'; readonly target: WorkflowMode }
  | { readonly kind: 'resume' };

/** Map from each artifact kind to the mode that produces it. */
export const KIND_TO_MODE: Readonly<Record<ArtifactKind, WorkflowMode>> = Object.fromEntries(
  WORKFLOW_MODES.map((mode) => [MODE_CONTRACTS[mode].producedArtifact, mode]),
) as Readonly<Record<ArtifactKind, WorkflowMode>>;

type EntryFailure =
  | { readonly type: 'paused'; readonly reason: string }
  | { readonly type: 'missing-input'; readonly kind: ArtifactKind; readonly reason: string }
  | { readonly type: 'stale-input'; readonly kind: ArtifactKind; readonly artifactId: string; readonly reason: string }
  | { readonly type: 'forward-skip'; readonly reason: string }
  | { readonly type: 'verification'; readonly reason: string };

type EntryCheck = { readonly ok: true } | { readonly ok: false; readonly failure: EntryFailure };

function verificationSummary(state: ProjectState): string {
  const verification = state.verification;
  if (verification.lastRunAt === null) return 'no verification run has been recorded yet';
  return `last run: ${verification.passedCount} passed, ${verification.failedCount} failed, ${verification.unevaluatedCount} unevaluated`;
}

function inspectModeEntry(state: ProjectState, target: WorkflowMode): EntryCheck {
  if (state.paused !== null) {
    return {
      ok: false,
      failure: {
        type: 'paused',
        reason: `workflow is paused (${state.paused.reason}); resume first with /resume`,
      },
    };
  }
  const contract = MODE_CONTRACTS[target];
  for (const kind of contract.requiredInputs) {
    const artifactId = state.artifactsByKind[kind];
    if (!artifactId) {
      return {
        ok: false,
        failure: {
          type: 'missing-input',
          kind,
          reason:
            `cannot enter ${target}: the required "${kind}" artifact does not exist yet; ` +
            `complete ${KIND_TO_MODE[kind]} first (currently in ${state.currentMode})`,
        },
      };
    }
    const artifact = state.artifacts[artifactId];
    if (!artifact) {
      return {
        ok: false,
        failure: {
          type: 'missing-input',
          kind,
          reason:
            `cannot enter ${target}: the required "${kind}" input points at artifact "${artifactId}" ` +
            'which is missing from project state; the event log may be corrupted',
        },
      };
    }
    if (artifact.status === 'stale') {
      return {
        ok: false,
        failure: {
          type: 'stale-input',
          kind,
          artifactId,
          reason:
            `cannot enter ${target}: the required "${kind}" artifact (${artifactId}) is stale; ` +
            'revise or regenerate it from its updated upstream inputs first',
        },
      };
    }
  }
  if (modeIndex(target) - modeIndex(state.currentMode) > 1) {
    return {
      ok: false,
      failure: {
        type: 'forward-skip',
        reason:
          `workflow modes run in order: cannot jump from ${state.currentMode} to ${target}; ` +
          'advance one mode at a time or use /revise to go back',
      },
    };
  }
  if (target === 'delivery' && state.verification.passed !== true) {
    return {
      ok: false,
      failure: {
        type: 'verification',
        reason:
          `cannot enter delivery: verification has not passed (${verificationSummary(state)}); ` +
          'complete the verification mode and address failing criteria first',
      },
    };
  }
  return { ok: true };
}

export function canEnterMode(state: ProjectState, target: WorkflowMode): { ok: boolean; reason: string } {
  const check = inspectModeEntry(state, target);
  return check.ok ? { ok: true, reason: `all requirements to enter ${target} are satisfied` } : { ok: false, reason: check.failure.reason };
}

/**
 * Earliest workflow stage affected by a revision of `revisedKind`: the lowest
 * mode whose produced artifact already exists and is transitively downstream
 * of the revised kind. When no downstream artifact exists yet, work resumes in
 * the mode that produces the revised kind itself.
 */
export function earliestAffectedStage(state: ProjectState, revisedKind: ArtifactKind): WorkflowMode {
  const downstream = new Set<string>(downstreamArtifactKinds(revisedKind));
  for (const mode of WORKFLOW_MODES) {
    const kind = MODE_CONTRACTS[mode].producedArtifact;
    if (state.artifactsByKind[kind] && downstream.has(kind)) {
      return mode;
    }
  }
  return KIND_TO_MODE[revisedKind];
}

function optionsForFailure(failure: EntryFailure): readonly string[] {
  switch (failure.type) {
    case 'paused':
      return ['/resume'];
    case 'stale-input':
      return ['/revise', '/status'];
    case 'verification':
      return ['/revise', '/inspect verification-report'];
    case 'forward-skip':
      return ['/revise', '/status'];
    case 'missing-input':
      return ['/status'];
  }
}

function needsUserFromFailure(failure: EntryFailure): ControllerOutcome {
  return {
    kind: 'NEEDS_USER',
    question: failure.reason,
    options: optionsForFailure(failure),
    reason: failure.reason,
  };
}

function advanceAfterGate(state: ProjectState, next: WorkflowMode, onAdvance: string): ControllerOutcome {
  const gate = inspectModeEntry(state, next);
  if (gate.ok) {
    return { kind: 'ADVANCE', to: next, reason: onAdvance };
  }
  if (gate.failure.type === 'stale-input') {
    return {
      kind: 'REVISE',
      stage: earliestAffectedStage(state, gate.failure.kind),
      reason: gate.failure.reason,
    };
  }
  return needsUserFromFailure(gate.failure);
}

export function evaluateTransition(state: ProjectState, trigger: TransitionTrigger): ControllerOutcome {
  switch (trigger.kind) {
    case 'mode-complete': {
      const contract = MODE_CONTRACTS[state.currentMode];
      const producedKind = contract.producedArtifact;
      const producedId = state.artifactsByKind[producedKind];
      if (!producedId) {
        return {
          kind: 'NEEDS_USER',
          question:
            `The ${state.currentMode} mode has not produced its "${producedKind}" artifact yet; ` +
            `finish that work before completing ${state.currentMode}.`,
          options: [`produce the ${producedKind}`, 'change mode'],
          reason: `${state.currentMode} is incomplete: no ${producedKind} artifact exists`,
        };
      }
      const produced = state.artifacts[producedId];
      if (isContractualArtifactKind(producedKind) && (!produced || produced.status !== 'approved')) {
        const version = produced ? produced.currentVersion : 1;
        return {
          kind: 'NEEDS_USER',
          question: `Approve the ${producedKind} (v${version}) to continue`,
          options: ['approve', 'request changes'],
          reason:
            `${producedKind} is a contractual checkpoint; its current status is ` +
            `${produced ? produced.status : 'unknown'} and only the user can approve it`,
        };
      }
      const next = contract.nextMode;
      if (!next) {
        return {
          kind: 'NEEDS_USER',
          question: 'Workflow complete; delivery actions require explicit authorization',
          options: ['review delivery report', 'authorize delivery'],
          reason: 'delivery mode is the final stage; external delivery actions need explicit user authorization',
        };
      }
      const evidence = trigger.evidence.length > 0 ? ` (${trigger.evidence.join('; ')})` : '';
      return advanceAfterGate(state, next, `${state.currentMode} is complete${evidence}; advancing to ${next}`);
    }

    case 'artifact-approved': {
      const artifact = state.artifacts[trigger.artifactId];
      if (!artifact) {
        return {
          kind: 'NEEDS_USER',
          question:
            `The approved artifact "${trigger.artifactId}" is not part of this project; ` +
            'check the artifact id with /status before approving.',
          options: ['/status'],
          reason: `artifact-approved trigger references unknown artifact "${trigger.artifactId}"`,
        };
      }
      const contract = MODE_CONTRACTS[state.currentMode];
      const producedKind = contract.producedArtifact;
      const completesCurrentMode =
        isContractualArtifactKind(producedKind) &&
        state.artifactsByKind[producedKind] === trigger.artifactId &&
        artifact.kind === producedKind;
      if (!completesCurrentMode) {
        return {
          kind: 'CONTINUE',
          reason:
            `approved ${artifact.kind} does not gate ${state.currentMode} ` +
            `(waiting on ${producedKind}); continuing current work`,
        };
      }
      const next = contract.nextMode;
      if (!next) {
        return {
          kind: 'NEEDS_USER',
          question: 'Workflow complete; delivery actions require explicit authorization',
          options: ['review delivery report', 'authorize delivery'],
          reason: 'delivery mode is the final stage; external delivery actions need explicit user authorization',
        };
      }
      return advanceAfterGate(state, next, `${artifact.kind} approved; advancing to ${next}`);
    }

    case 'verification-failed': {
      return {
        kind: 'REVISE',
        stage: trigger.earliestAffectedStage,
        reason:
          `verification failed; routing back to ${trigger.earliestAffectedStage} ` +
          '(the earliest stage affected by the failing work) to address the failing criteria',
      };
    }

    case 'user-request': {
      const gate = inspectModeEntry(state, trigger.target);
      if (gate.ok) {
        return { kind: 'ADVANCE', to: trigger.target, reason: `user override (from ${state.currentMode} to ${trigger.target})` };
      }
      return needsUserFromFailure(gate.failure);
    }

    case 'resume': {
      if (state.paused !== null) {
        return {
          kind: 'NEEDS_USER',
          question: state.paused.question ?? 'resumed; continue',
          options: state.paused.options.length > 0 ? [...state.paused.options] : ['continue'],
          reason: `workflow is paused (${state.paused.reason}); answer the pause to continue`,
        };
      }
      return { kind: 'CONTINUE', reason: 'resumed; continue' };
    }
  }
}

/**
 * Whether the controller may keep advancing without asking the user, bounded
 * by the current autonomy setting. Never true while paused.
 */
export function shouldAutoAdvance(state: ProjectState, outcome: ControllerOutcome): boolean {
  if (state.paused !== null) return false;
  switch (state.autonomy) {
    case 'off':
      return false;
    case 'next-checkpoint':
      return outcome.kind === 'ADVANCE';
    case 'workflow':
      return outcome.kind === 'ADVANCE' || outcome.kind === 'CONTINUE';
    default: {
      const target = autonomyTarget(state.autonomy);
      return target !== null && state.currentMode !== target;
    }
  }
}
