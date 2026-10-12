/**
 * User-intervention policy.
 *
 * Answers one question: does this action require an explicit human decision
 * before Relay may proceed? Approvals of contractual artifacts, changes to
 * workflow direction or autonomy, destructive operations, deployment,
 * delivery, scope expansion beyond the approved plan, and consequential
 * product choices all require the user. Routine reversible work does not.
 */

import type { ProjectState } from '../core/project-state.js';

export type InterventionAction =
  | 'approve-artifact'
  | 'revise-artifact'
  | 'change-mode'
  | 'set-autonomy'
  | 'destructive-operation'
  | 'scope-expansion'
  | 'deployment'
  | 'delivery'
  | 'product-choice';

export function requiresUserIntervention(
  state: ProjectState,
  action: InterventionAction,
  detail?: string,
): { required: boolean; reason: string } {
  switch (action) {
    case 'approve-artifact':
      return {
        required: true,
        reason:
          `approving a contractual artifact while in ${state.currentMode} makes it the binding ` +
          'contract for every later stage; the user must approve it explicitly',
      };
    case 'revise-artifact':
      return {
        required: true,
        reason:
          `revising an artifact invalidates downstream work derived from it (currently in ${state.currentMode}); ` +
          'the user must request the revision explicitly',
      };
    case 'change-mode':
      return {
        required: true,
        reason:
          `changing the workflow mode (currently ${state.currentMode}) redirects all subsequent work; ` +
          'the user must choose the target mode',
      };
    case 'set-autonomy':
      return {
        required: true,
        reason:
          `changing the autonomy bound (currently ${state.autonomy}) changes how far Relay runs without asking; ` +
          'the user must set it',
      };
    case 'destructive-operation':
      return {
        required: true,
        reason:
          'destructive operations can irreversibly destroy user work or system state; ' +
          'they require explicit user authorization',
      };
    case 'scope-expansion': {
      const trimmed = typeof detail === 'string' ? detail.trim() : '';
      if (trimmed === '') {
        return {
          required: false,
          reason: 'no out-of-scope work detected; staying within the approved execution plan requires no intervention',
        };
      }
      return {
        required: true,
        reason: `work expanded beyond the approved execution plan: ${trimmed}; the user must approve the scope change`,
      };
    }
    case 'deployment':
      return {
        required: true,
        reason:
          'deployment affects systems outside the local workspace and cannot be undone from here; ' +
          'it requires explicit user authorization',
      };
    case 'delivery':
      return {
        required: true,
        reason: 'delivery actions (commit, push, PR, deploy, publish) require explicit user authorization',
      };
    case 'product-choice':
      return {
        required: true,
        reason:
          'consequential product decisions shape the approved artifacts and the delivered result; ' +
          'the user must make them',
      };
  }
}
