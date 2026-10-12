/**
 * Policy facade over the core transition rules.
 *
 * `evaluatePolicyTransition` composes the deterministic core transition policy
 * with the intervention policy: completing the delivery mode must always
 * surface a NEEDS_USER outcome citing the delivery intervention rule.
 * `assertTransitionAllowed` turns `canEnterMode` rejections into thrown
 * {@link PolicyRejectionError}s the controller can record as
 * `mode.transition.rejected` events.
 */

import type { WorkflowMode } from '../core/modes.js';
import type { ControllerOutcome } from '../core/outcomes.js';
import type { ProjectState } from '../core/project-state.js';
import { canEnterMode, evaluateTransition, type TransitionTrigger } from '../core/transitions.js';
import { requiresUserIntervention } from './intervention.js';

/** Thrown when a mode transition is rejected by policy; `reason` is user-facing. */
export class PolicyRejectionError extends Error {
  constructor(
    readonly reason: string,
  ) {
    super(reason);
    this.name = 'PolicyRejectionError';
  }
}

export function evaluatePolicyTransition(state: ProjectState, trigger: TransitionTrigger): ControllerOutcome {
  if (trigger.kind === 'mode-complete' && state.currentMode === 'delivery') {
    const base = evaluateTransition(state, trigger);
    const intervention = requiresUserIntervention(state, 'delivery');
    if (base.kind === 'NEEDS_USER') {
      return { ...base, reason: `${base.reason}; policy: ${intervention.reason}` };
    }
    return {
      kind: 'NEEDS_USER',
      question: 'Workflow complete; delivery actions require explicit authorization',
      options: ['review delivery report', 'authorize delivery'],
      reason: intervention.reason,
    };
  }
  return evaluateTransition(state, trigger);
}

/** Throw {@link PolicyRejectionError} unless `target` may be entered right now. */
export function assertTransitionAllowed(state: ProjectState, target: WorkflowMode): void {
  const check = canEnterMode(state, target);
  if (!check.ok) {
    throw new PolicyRejectionError(check.reason);
  }
}
