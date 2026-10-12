/**
 * Workflow outcome contracts.
 *
 * After every controller step the deterministic transition policy produces
 * exactly one outcome. Models may propose transitions; only policy code
 * emits outcomes.
 */

import type { WorkflowMode } from './modes.js';

export type OutcomeKind = 'CONTINUE' | 'ADVANCE' | 'NEEDS_USER' | 'REVISE';

export type ControllerOutcome =
  | { readonly kind: 'CONTINUE'; readonly reason: string }
  | { readonly kind: 'ADVANCE'; readonly to: WorkflowMode; readonly reason: string }
  | {
    readonly kind: 'NEEDS_USER';
    readonly question: string;
    readonly options: readonly string[];
    readonly reason: string;
  }
  | { readonly kind: 'REVISE'; readonly stage: WorkflowMode; readonly reason: string };

export function outcomeKind(outcome: ControllerOutcome): OutcomeKind {
  return outcome.kind;
}
