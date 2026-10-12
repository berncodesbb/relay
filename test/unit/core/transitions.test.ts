import { describe, expect, it } from 'vitest';
import type { AnyRelayEvent } from '../../../src/core/events.js';
import {
  canEnterMode,
  earliestAffectedStage,
  evaluateTransition,
  KIND_TO_MODE,
  shouldAutoAdvance,
} from '../../../src/core/transitions.js';
import {
  artifactApproved,
  artifactCreated,
  artifactRevised,
  fullPipelineApproved,
  initialized,
  makeEvent,
  modeEntered,
  replay,
} from './event-factories.js';

function stateOf(events: readonly AnyRelayEvent[]) {
  return replay(events);
}

const pausedEvents = (): AnyRelayEvent[] => [
  initialized(),
  makeEvent({ type: 'workflow.paused', projectId: 'proj_test', payload: { by: 'user', reason: 'reviewing', question: null, options: [] } }),
];

describe('canEnterMode', () => {
  it('requires the mode contract inputs to exist', () => {
    const state = stateOf([initialized()]);
    const check = canEnterMode(state, 'specification');
    expect(check.ok).toBe(false);
    expect(check.reason).toContain('opportunity-brief');
  });

  it('rejects stale inputs, naming kind and artifact id', () => {
    const state = stateOf([
      ...fullPipelineApproved().slice(0, 14),
      artifactRevised('art-trd', 'trd', 1, 'architecture fix'),
    ]);
    // Entering verification requires a fresh execution-plan; it went stale with the TRD revision.
    const check = canEnterMode(state, 'verification');
    expect(check.ok).toBe(false);
    expect(check.reason).toContain('execution-plan');
    expect(check.reason).toContain('art-plan');
  });

  it('allows entering the next mode and any earlier mode', () => {
    const inSpecification = stateOf([
      initialized(),
      artifactCreated('opportunity-brief', 'art-brief'),
      artifactApproved('art-brief', 'opportunity-brief'),
      modeEntered('specification', 'discovery', 'auto', 'brief approved'),
    ]);
    expect(canEnterMode(inSpecification, 'technical-design').ok).toBe(false); // no PRD yet

    const inPlanning = stateOf(fullPipelineApproved().slice(0, 12)); // through planning entry
    expect(canEnterMode(inPlanning, 'specification').ok).toBe(true);
    expect(canEnterMode(inPlanning, 'implementation').ok).toBe(true);
  });

  it('rejects forward jumps beyond the next mode', () => {
    const state = stateOf([
      initialized(),
      artifactCreated('opportunity-brief', 'art-brief'),
      artifactApproved('art-brief', 'opportunity-brief'),
      modeEntered('specification', 'discovery', 'auto', 'brief approved'),
      artifactCreated('prd', 'art-prd'),
      artifactApproved('art-prd', 'prd'),
      artifactCreated('trd', 'art-trd'),
      artifactApproved('art-trd', 'trd'),
    ]);
    // All planning inputs exist and are fresh, but planning is two modes ahead.
    const check = canEnterMode(state, 'planning');
    expect(check.ok).toBe(false);
    expect(check.reason).toContain('workflow modes run in order');
    expect(check.reason).toContain('/revise');
  });

  it('requires verification.passed to enter delivery', () => {
    const failed = stateOf([
      ...fullPipelineApproved().slice(0, 16),
      makeEvent({
        type: 'verification.completed',
        projectId: 'proj_test',
        payload: { passed: false, results: [{ criterion: 'c', status: 'fail', evidence: 'e' }], reportArtifactId: 'art-vr' },
      }),
    ]);
    const check = canEnterMode(failed, 'delivery');
    expect(check.ok).toBe(false);
    expect(check.reason).toContain('verification has not passed');

    const passed = stateOf(fullPipelineApproved().slice(0, 18));
    expect(canEnterMode(passed, 'delivery').ok).toBe(true);
  });

  it('rejects entering any mode while paused', () => {
    const state = stateOf(pausedEvents());
    const check = canEnterMode(state, 'specification');
    expect(check.ok).toBe(false);
    expect(check.reason).toContain('/resume');
  });
});

describe('evaluateTransition: mode-complete', () => {
  it('asks the user to produce the missing mode artifact', () => {
    const state = stateOf([initialized()]);
    const outcome = evaluateTransition(state, { kind: 'mode-complete', evidence: [] });
    expect(outcome.kind).toBe('NEEDS_USER');
    if (outcome.kind === 'NEEDS_USER') {
      expect(outcome.question).toContain('opportunity-brief');
      expect(outcome.options.length).toBeGreaterThan(0);
    }
  });

  it('gates advancement on approval of the contractual artifact', () => {
    const state = stateOf([initialized(), artifactCreated('opportunity-brief', 'art-brief')]);
    const outcome = evaluateTransition(state, { kind: 'mode-complete', evidence: ['research done'] });
    expect(outcome).toEqual({
      kind: 'NEEDS_USER',
      question: 'Approve the opportunity-brief (v1) to continue',
      options: ['approve', 'request changes'],
      reason: expect.stringContaining('contractual'),
    });
  });

  it('advances once the contractual artifact is approved', () => {
    const state = stateOf([initialized(), artifactCreated('opportunity-brief', 'art-brief'), artifactApproved('art-brief', 'opportunity-brief')]);
    const outcome = evaluateTransition(state, { kind: 'mode-complete', evidence: ['research done'] });
    expect(outcome).toEqual({
      kind: 'ADVANCE',
      to: 'specification',
      reason: expect.stringContaining('research done'),
    });
  });

  it('does not gate non-contractual mode artifacts on approval', () => {
    // Implementation produces a decision log, which is not contractual.
    const state = stateOf(fullPipelineApproved().slice(0, 14));
    const outcome = evaluateTransition(state, { kind: 'mode-complete', evidence: ['tasks done'] });
    expect(outcome.kind).toBe('ADVANCE');
    if (outcome.kind === 'ADVANCE') expect(outcome.to).toBe('verification');
  });

  it('routes stale inputs back to the earliest affected stage instead of advancing', () => {
    // In implementation with the decision log produced, but the plan went stale.
    const state = stateOf([
      ...fullPipelineApproved().slice(0, 14),
      artifactRevised('art-trd', 'trd', 1, 'architecture fix'),
    ]);
    const outcome = evaluateTransition(state, { kind: 'mode-complete', evidence: ['tasks done'] });
    expect(outcome).toEqual({
      kind: 'REVISE',
      stage: 'planning',
      reason: expect.stringContaining('stale'),
    });
  });

  it('ends in NEEDS_USER when delivery completes', () => {
    const state = stateOf(fullPipelineApproved());
    const outcome = evaluateTransition(state, { kind: 'mode-complete', evidence: ['report assembled'] });
    expect(outcome.kind).toBe('NEEDS_USER');
    if (outcome.kind === 'NEEDS_USER') {
      expect(outcome.question).toBe('Workflow complete; delivery actions require explicit authorization');
    }
  });

  it('surfaces pause rejections as NEEDS_USER', () => {
    const state = stateOf([
      ...pausedEvents(),
      artifactCreated('opportunity-brief', 'art-brief'),
      artifactApproved('art-brief', 'opportunity-brief'),
    ]);
    const outcome = evaluateTransition(state, { kind: 'mode-complete', evidence: [] });
    expect(outcome.kind).toBe('NEEDS_USER');
    if (outcome.kind === 'NEEDS_USER') {
      expect(outcome.question).toContain('/resume');
    }
  });
});

describe('evaluateTransition: artifact-approved', () => {
  it('advances after the current mode artifact is approved', () => {
    const state = stateOf([initialized(), artifactCreated('opportunity-brief', 'art-brief')]);
    const outcome = evaluateTransition(state, { kind: 'artifact-approved', artifactId: 'art-brief' });
    expect(outcome).toEqual({
      kind: 'ADVANCE',
      to: 'specification',
      reason: expect.stringContaining('opportunity-brief'),
    });
  });

  it('continues when the approved artifact does not gate the current mode', () => {
    const state = stateOf(fullPipelineApproved().slice(0, 5)); // in specification, PRD not yet created
    const outcome = evaluateTransition(state, { kind: 'artifact-approved', artifactId: 'art-brief' });
    expect(outcome.kind).toBe('CONTINUE');
  });

  it('asks the user when the referenced artifact is unknown', () => {
    const state = stateOf([initialized()]);
    const outcome = evaluateTransition(state, { kind: 'artifact-approved', artifactId: 'ghost' });
    expect(outcome.kind).toBe('NEEDS_USER');
    if (outcome.kind === 'NEEDS_USER') expect(outcome.question).toContain('ghost');
  });

  it('routes to REVISE when the next mode is blocked by stale inputs', () => {
    // Brief revised while in technical-design: prd/trd went stale. The TRD is
    // re-approved as the current mode artifact, but entering planning is
    // blocked by the stale PRD.
    const state = stateOf([
      ...fullPipelineApproved().slice(0, 9),
      artifactRevised('art-brief', 'opportunity-brief', 1, 'problem reframed'),
      artifactApproved('art-trd', 'trd'),
    ]);
    const outcome = evaluateTransition(state, { kind: 'artifact-approved', artifactId: 'art-trd' });
    expect(outcome.kind).toBe('REVISE');
    if (outcome.kind === 'REVISE') expect(outcome.stage).toBe('technical-design');
  });
});

describe('evaluateTransition: verification-failed, user-request, resume', () => {
  it('routes verification failures back to the trigger-provided stage', () => {
    const state = stateOf(fullPipelineApproved().slice(0, 18));
    const outcome = evaluateTransition(state, { kind: 'verification-failed', earliestAffectedStage: 'specification' });
    expect(outcome).toEqual({
      kind: 'REVISE',
      stage: 'specification',
      reason: expect.stringContaining('verification failed'),
    });
  });

  it('honors valid user requests with a user-override advance', () => {
    const state = stateOf(fullPipelineApproved().slice(0, 12)); // in planning
    const outcome = evaluateTransition(state, { kind: 'user-request', target: 'specification' });
    expect(outcome).toEqual({
      kind: 'ADVANCE',
      to: 'specification',
      reason: expect.stringContaining('user override'),
    });
  });

  it('surfaces rejected user requests with the rejection reason', () => {
    const state = stateOf([initialized()]);
    const outcome = evaluateTransition(state, { kind: 'user-request', target: 'delivery' });
    expect(outcome.kind).toBe('NEEDS_USER');
    if (outcome.kind === 'NEEDS_USER') {
      expect(outcome.question).toContain('cannot enter delivery');
    }
  });

  it('re-presents the pause question on resume while paused', () => {
    const state = stateOf([
      initialized(),
      makeEvent({
        type: 'workflow.paused',
        projectId: 'proj_test',
        payload: { by: 'policy', reason: 'approval needed', question: 'Approve the prd (v1)?', options: ['approve', 'request changes'] },
      }),
    ]);
    const outcome = evaluateTransition(state, { kind: 'resume' });
    expect(outcome.kind).toBe('NEEDS_USER');
    if (outcome.kind === 'NEEDS_USER') {
      expect(outcome.question).toBe('Approve the prd (v1)?');
      expect(outcome.options).toEqual(['approve', 'request changes']);
    }
  });

  it('uses the generic message for a questionless pause and continues when unpaused', () => {
    const paused = stateOf([
      initialized(),
      makeEvent({ type: 'workflow.paused', projectId: 'proj_test', payload: { by: 'user', reason: 'lunch', question: null, options: [] } }),
    ]);
    const outcome = evaluateTransition(paused, { kind: 'resume' });
    expect(outcome.kind).toBe('NEEDS_USER');
    if (outcome.kind === 'NEEDS_USER') expect(outcome.question).toBe('resumed; continue');

    const active = stateOf([initialized()]);
    expect(evaluateTransition(active, { kind: 'resume' })).toEqual({ kind: 'CONTINUE', reason: 'resumed; continue' });
  });
});

describe('earliestAffectedStage and KIND_TO_MODE', () => {
  const complete = stateOf(fullPipelineApproved());

  it('maps each kind to the mode that produces it', () => {
    expect(KIND_TO_MODE['opportunity-brief']).toBe('discovery');
    expect(KIND_TO_MODE.prd).toBe('specification');
    expect(KIND_TO_MODE.trd).toBe('technical-design');
    expect(KIND_TO_MODE['execution-plan']).toBe('planning');
    expect(KIND_TO_MODE['decision-log']).toBe('implementation');
    expect(KIND_TO_MODE['verification-report']).toBe('verification');
    expect(KIND_TO_MODE['delivery-report']).toBe('delivery');
  });

  it('returns the earliest mode with an existing downstream artifact', () => {
    expect(earliestAffectedStage(complete, 'opportunity-brief')).toBe('specification');
    expect(earliestAffectedStage(complete, 'prd')).toBe('technical-design');
    expect(earliestAffectedStage(complete, 'trd')).toBe('planning');
    expect(earliestAffectedStage(complete, 'execution-plan')).toBe('verification');
  });

  it('falls back to the mode producing the revised kind when nothing downstream exists', () => {
    const onlyPrd = stateOf([initialized(), artifactCreated('prd', 'art-prd')]);
    expect(earliestAffectedStage(onlyPrd, 'prd')).toBe('specification');
    const onlyBrief = stateOf([initialized(), artifactCreated('opportunity-brief', 'art-brief')]);
    expect(earliestAffectedStage(onlyBrief, 'opportunity-brief')).toBe('discovery');
  });
});

describe('shouldAutoAdvance', () => {
  const advance = { kind: 'ADVANCE', to: 'specification', reason: 'r' } as const;
  const continueOutcome = { kind: 'CONTINUE', reason: 'r' } as const;
  const needsUser = { kind: 'NEEDS_USER', question: 'q', options: [], reason: 'r' } as const;

  const stateWith = (autonomy: string, events?: readonly AnyRelayEvent[]) => {
    const extra = events ?? [];
    return replay([initialized('goal', autonomy), ...extra]);
  };

  it('never auto-advances when autonomy is off', () => {
    const state = stateWith('off');
    expect(shouldAutoAdvance(state, advance)).toBe(false);
    expect(shouldAutoAdvance(state, continueOutcome)).toBe(false);
  });

  it('auto-advances exactly one step per checkpoint at next-checkpoint', () => {
    const state = stateWith('next-checkpoint');
    expect(shouldAutoAdvance(state, advance)).toBe(true);
    expect(shouldAutoAdvance(state, continueOutcome)).toBe(false);
    expect(shouldAutoAdvance(state, needsUser)).toBe(false);
  });

  it('auto-advances through the whole workflow at workflow autonomy', () => {
    const state = stateWith('workflow');
    expect(shouldAutoAdvance(state, advance)).toBe(true);
    expect(shouldAutoAdvance(state, continueOutcome)).toBe(true);
    expect(shouldAutoAdvance(state, needsUser)).toBe(false);
  });

  it('runs until the until:<mode> bound is reached', () => {
    const before = stateWith('until:planning');
    expect(shouldAutoAdvance(before, advance)).toBe(true);
    expect(shouldAutoAdvance(before, continueOutcome)).toBe(true);

    const atTarget = stateWith('until:planning', [modeEntered('planning', 'technical-design', 'auto', 'trd approved')]);
    expect(shouldAutoAdvance(atTarget, advance)).toBe(false);
  });

  it('never auto-advances while paused', () => {
    const state = stateWith('workflow', [
      makeEvent({ type: 'workflow.paused', projectId: 'proj_test', payload: { by: 'policy', reason: 'checkpoint', question: null, options: [] } }),
    ]);
    expect(shouldAutoAdvance(state, advance)).toBe(false);
    expect(shouldAutoAdvance(state, continueOutcome)).toBe(false);
  });
});
