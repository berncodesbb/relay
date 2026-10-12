import { describe, expect, it } from 'vitest';
import { fullPipelineApproved, initialized, replay } from '../core/event-factories.js';
import { requiresUserIntervention, type InterventionAction } from '../../../src/policies/intervention.js';

const state = replay(fullPipelineApproved());

const alwaysRequired: readonly InterventionAction[] = [
  'approve-artifact',
  'revise-artifact',
  'change-mode',
  'set-autonomy',
  'destructive-operation',
  'deployment',
  'delivery',
  'product-choice',
];

describe('requiresUserIntervention', () => {
  for (const action of alwaysRequired) {
    it(`requires the user for ${action}`, () => {
      const result = requiresUserIntervention(state, action);
      expect(result.required).toBe(true);
      expect(result.reason.length).toBeGreaterThan(10);
    });
  }

  it('explains delivery authorization in terms of the concrete actions', () => {
    const result = requiresUserIntervention(state, 'delivery');
    expect(result.reason).toBe(
      'delivery actions (commit, push, PR, deploy, publish) require explicit user authorization',
    );
  });

  it('anchors approval and mode-change reasons in the current mode', () => {
    const inDiscovery = replay([initialized()]);
    expect(requiresUserIntervention(inDiscovery, 'approve-artifact').reason).toContain('discovery');
    expect(requiresUserIntervention(inDiscovery, 'change-mode').reason).toContain('discovery');
    expect(requiresUserIntervention(inDiscovery, 'set-autonomy').reason).toContain(inDiscovery.autonomy);
  });

  it('requires scope expansion only when out-of-scope detail is provided', () => {
    expect(requiresUserIntervention(state, 'scope-expansion').required).toBe(false);
    expect(requiresUserIntervention(state, 'scope-expansion', '').required).toBe(false);
    expect(requiresUserIntervention(state, 'scope-expansion', '   ').required).toBe(false);

    const flagged = requiresUserIntervention(state, 'scope-expansion', 'touches src/extra-module.ts not in the plan');
    expect(flagged.required).toBe(true);
    expect(flagged.reason).toContain('src/extra-module.ts');
  });
});
