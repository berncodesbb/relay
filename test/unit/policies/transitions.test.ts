import { describe, expect, it } from 'vitest';
import { evaluateTransition } from '../../../src/core/transitions.js';
import {
  artifactApproved,
  artifactCreated,
  fullPipelineApproved,
  initialized,
  replay,
} from '../core/event-factories.js';
import {
  assertTransitionAllowed,
  evaluatePolicyTransition,
  PolicyRejectionError,
} from '../../../src/policies/transitions.js';

describe('evaluatePolicyTransition', () => {
  it('delegates to the core transition policy for ordinary triggers', () => {
    const state = replay([
      initialized(),
      artifactCreated('opportunity-brief', 'art-brief'),
      artifactApproved('art-brief', 'opportunity-brief'),
    ]);
    const trigger = { kind: 'mode-complete', evidence: ['research done'] } as const;
    expect(evaluatePolicyTransition(state, trigger)).toEqual(evaluateTransition(state, trigger));
  });

  it('forces completing delivery into NEEDS_USER citing the delivery intervention', () => {
    const state = replay(fullPipelineApproved());
    const outcome = evaluatePolicyTransition(state, { kind: 'mode-complete', evidence: ['report assembled'] });
    expect(outcome.kind).toBe('NEEDS_USER');
    if (outcome.kind === 'NEEDS_USER') {
      expect(outcome.reason).toContain(
        'delivery actions (commit, push, PR, deploy, publish) require explicit user authorization',
      );
    }
  });

  it('keeps the delivery intervention citation even when the base outcome differs', () => {
    // Delivery mode with no delivery report yet: the base outcome asks to
    // produce the artifact; the policy still cites the delivery intervention.
    const state = replay(fullPipelineApproved().slice(0, 19));
    const outcome = evaluatePolicyTransition(state, { kind: 'mode-complete', evidence: [] });
    expect(outcome.kind).toBe('NEEDS_USER');
    if (outcome.kind === 'NEEDS_USER') {
      expect(outcome.question).toContain('delivery-report');
      expect(outcome.reason).toContain('delivery actions (commit, push, PR, deploy, publish)');
    }
  });
});

describe('assertTransitionAllowed', () => {
  it('passes silently when the target mode may be entered', () => {
    const state = replay([
      initialized(),
      artifactCreated('opportunity-brief', 'art-brief'),
      artifactApproved('art-brief', 'opportunity-brief'),
    ]);
    expect(() => assertTransitionAllowed(state, 'specification')).not.toThrow();
  });

  it('throws PolicyRejectionError carrying the actionable reason', () => {
    const state = replay([initialized()]);
    let thrown: PolicyRejectionError | null = null;
    try {
      assertTransitionAllowed(state, 'specification');
    } catch (error) {
      thrown = error as PolicyRejectionError;
    }
    expect(thrown).toBeInstanceOf(PolicyRejectionError);
    expect(thrown!.name).toBe('PolicyRejectionError');
    expect(thrown!.reason).toContain('opportunity-brief');
    expect(thrown!.message).toBe(thrown!.reason);
  });
});
