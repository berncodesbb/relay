import { describe, expect, it } from 'vitest';
import { UnknownEventTypeError, type AnyRelayEvent } from '../../../src/core/events.js';
import { applyEvent, EventFoldError, replayProject } from '../../../src/core/reducer.js';
import { createEmptyProjectState } from '../../../src/core/project-state.js';
import {
  artifactApproved,
  artifactCreated,
  artifactRevised,
  fullPipelineApproved,
  initialized,
  makeEvent,
  modeEntered,
  replay,
  resetEventFactories,
  verificationCompleted,
} from './event-factories.js';

describe('reducer: initialization', () => {
  it('folds project.initialized into fresh state', () => {
    const event = initialized('Build a CLI', 'until:planning');
    const state = applyEvent(createEmptyProjectState(), event);
    expect(state.projectId).toBe('proj_test');
    expect(state.goal).toBe('Build a CLI');
    expect(state.workspaceRoot).toBe('/tmp/relay-workspace');
    expect(state.initializedAt).toBe(event.timestamp);
    expect(state.currentMode).toBe('discovery');
    expect(state.autonomy).toBe('until:planning');
    expect(state.stats.events).toBe(1);
    expect(state.lastActivityAt).toBe(event.timestamp);
  });

  it('falls back to next-checkpoint for an unparseable initial autonomy', () => {
    expect(replay([initialized('goal', 'yolo')]).autonomy).toBe('next-checkpoint');
  });

  it('falls back to next-checkpoint when initialAutonomy is empty', () => {
    expect(replay([initialized('goal', '')]).autonomy).toBe('next-checkpoint');
  });

  it('rejects a duplicate project.initialized', () => {
    const events = [
      initialized(),
      makeEvent({ type: 'session.started', projectId: 'proj_test', payload: { sessionId: 's1', interactive: true } }),
      initialized('another goal'),
    ];
    expect(() => replay(events)).toThrow(EventFoldError);
    expect(() => replay(events)).toThrow(/duplicate project\.initialized/);
  });
});

describe('reducer: event effects', () => {
  it('activity-only events bump counters and lastActivityAt only', () => {
    const events: AnyRelayEvent[] = [
      initialized(),
      makeEvent({ type: 'session.started', projectId: 'proj_test', payload: { sessionId: 's1', interactive: true } }),
      makeEvent({
        type: 'intent.proposed',
        projectId: 'proj_test',
        payload: {
          source: 'natural-language',
          raw: 'go faster',
          proposal: { kind: 'advance', confidence: 0.9, rationale: 'clear request' },
        },
      }),
      makeEvent({
        type: 'intent.applied',
        projectId: 'proj_test',
        payload: { kind: 'advance', source: 'natural-language', confidence: 0.9, mode: null, autonomy: null, artifactKind: null, overridden: false },
      }),
      makeEvent({ type: 'artifact.approval.requested', projectId: 'proj_test', payload: { artifactId: 'a1', kind: 'prd', version: 1, reason: 'ready for review' } }),
      makeEvent({ type: 'artifact.inspected', projectId: 'proj_test', payload: { artifactId: 'a1', via: 'user' } }),
      makeEvent({ type: 'model.requested', projectId: 'proj_test', payload: { provider: 'openai', model: 'gpt-x', purpose: 'draft', mode: 'discovery', attempt: 1 } }),
      makeEvent({ type: 'tool.denied', projectId: 'proj_test', payload: { tool: 'shell', capability: 'execute', mode: 'discovery', reason: 'not permitted' } }),
    ];
    const state = replay(events);
    expect(state.stats.events).toBe(events.length);
    expect(state.lastActivityAt).toBe(events[events.length - 1]!.timestamp);
    expect(state.stats.modelCalls).toBe(0);
    expect(state.stats.toolCalls).toBe(0);
    expect(state.artifacts).toEqual({});
    expect(state.paused).toBeNull();
  });

  it('folds mode.entered with its transition record', () => {
    const state = replay([initialized(), modeEntered('specification', 'discovery', 'user', 'brief approved')]);
    expect(state.currentMode).toBe('specification');
    expect(state.transitions).toHaveLength(1);
    expect(state.transitions[0]).toMatchObject({
      from: 'discovery',
      to: 'specification',
      reason: 'brief approved',
      source: 'user',
      at: state.lastActivityAt,
    });
  });

  it('caps rejected transitions at 50, dropping the oldest', () => {
    const events: AnyRelayEvent[] = [initialized()];
    for (let i = 0; i < 60; i += 1) {
      events.push(
        makeEvent({
          type: 'mode.transition.rejected',
          projectId: 'proj_test',
          payload: { from: 'discovery', to: 'delivery', reason: `rejection ${i}` },
        }),
      );
    }
    const state = replay(events);
    expect(state.rejectedTransitions).toHaveLength(50);
    expect(state.rejectedTransitions[0]!.reason).toBe('rejection 10');
    expect(state.rejectedTransitions[49]!.reason).toBe('rejection 59');
  });

  it('folds autonomy.changed, falling back on invalid bounds', () => {
    const base = [initialized('goal', 'off')];
    const changed = replay([
      ...base,
      makeEvent({ type: 'autonomy.changed', projectId: 'proj_test', payload: { from: 'off', to: 'until:verification', source: 'user' } }),
    ]);
    expect(changed.autonomy).toBe('until:verification');

    const invalid = replay([
      ...base,
      makeEvent({ type: 'autonomy.changed', projectId: 'proj_test', payload: { from: 'off', to: 'whatever', source: 'user' } }),
    ]);
    expect(invalid.autonomy).toBe('next-checkpoint');
  });

  it('folds artifact.created as a draft at version 1 and indexes it by kind', () => {
    const state = replay([initialized(), artifactCreated('opportunity-brief', 'art-brief', 'My brief')]);
    const artifact = state.artifacts['art-brief'];
    expect(artifact).toBeDefined();
    expect(artifact).toMatchObject({
      artifactId: 'art-brief',
      kind: 'opportunity-brief',
      title: 'My brief',
      currentVersion: 1,
      status: 'draft',
      path: '.relay/artifacts/opportunity-brief-art-brief.md',
      approvedAt: null,
      revisions: [],
    });
    expect(artifact!.createdAt).toBe(artifact!.lastChangedAt);
    expect(state.artifactsByKind['opportunity-brief']).toBe('art-brief');
  });

  it('rejects artifact.created reusing an existing artifact id', () => {
    const events = [initialized(), artifactCreated('prd', 'art-prd'), artifactCreated('prd', 'art-prd', 'duplicate')];
    expect(() => replay(events)).toThrow(/reuses existing artifact id "art-prd"/);
  });

  it('folds artifact.approved with approvedAt and clears it from staleArtifacts', () => {
    const state = replay([
      initialized(),
      artifactCreated('prd', 'art-prd'),
      makeEvent({
        type: 'artifact.invalidated',
        projectId: 'proj_test',
        payload: { artifactIds: ['art-prd'], cause: { kind: 'opportunity-brief', artifactId: 'art-brief', version: 2 }, reason: 'upstream revised' },
      }),
      artifactApproved('art-prd', 'prd'),
    ]);
    expect(state.artifacts['art-prd']!.status).toBe('approved');
    expect(state.artifacts['art-prd']!.approvedAt).toBe(state.lastActivityAt);
    expect(state.staleArtifacts).toEqual([]);
  });

  it('rejects approving a version other than the current one', () => {
    const events = [
      initialized(),
      artifactCreated('prd', 'art-prd'),
      artifactApproved('art-prd', 'prd'),
      artifactRevised('art-prd', 'prd', 1, 'tighten scope'),
      artifactApproved('art-prd', 'prd', 1),
    ];
    expect(() => replay(events)).toThrow(EventFoldError);
    expect(() => replay(events)).toThrow(/targets prd "art-prd" at version 1, but the current version is 2/);
  });

  it('rejects revisions that do not increment the version by exactly one', () => {
    const bad = [
      initialized(),
      artifactCreated('prd', 'art-prd'),
      makeEvent({
        type: 'artifact.revised',
        projectId: 'proj_test',
        payload: { artifactId: 'art-prd', kind: 'prd', fromVersion: 1, toVersion: 3, path: '.relay/artifacts/prd.md', reason: 'skip a version' },
      }),
    ];
    expect(() => replay(bad)).toThrow(/to version 3, but the current version is 1/);
  });

  it('rejects revisions and approvals referencing unknown artifacts', () => {
    const revised = [
      initialized(),
      makeEvent({
        type: 'artifact.revised',
        projectId: 'proj_test',
        payload: { artifactId: 'ghost', kind: 'prd', fromVersion: 1, toVersion: 2, path: 'x.md', reason: 'r' },
      }),
    ];
    expect(() => replay(revised)).toThrow(/unknown artifact "ghost"/);

    const approved = [initialized(), artifactApproved('ghost', 'prd')];
    expect(() => replay(approved)).toThrow(/unknown artifact "ghost"/);
  });

  it('folds model and tool telemetry into stats', () => {
    const state = replay([
      initialized(),
      makeEvent({
        type: 'model.responded',
        projectId: 'proj_test',
        payload: { provider: 'openai', model: 'gpt-x', purpose: 'draft', usage: { inputTokens: 10, outputTokens: 5 }, toolCalls: 0, durationMs: 100 },
      }),
      makeEvent({
        type: 'model.responded',
        projectId: 'proj_test',
        payload: { provider: 'openai', model: 'gpt-x', purpose: 'draft', usage: { inputTokens: 10, outputTokens: 5 }, toolCalls: 0, durationMs: 100 },
      }),
      makeEvent({
        type: 'model.failed',
        projectId: 'proj_test',
        payload: { provider: 'openai', model: 'gpt-x', purpose: 'draft', errorKind: 'rate-limit', message: 'throttled after retries', attempts: 3, durationMs: 900 },
      }),
      makeEvent({
        type: 'tool.executed',
        projectId: 'proj_test',
        payload: { tool: 'read-file', capability: 'read', mode: 'discovery', outcome: 'ok', durationMs: 5, summary: 'read a file', affectedFiles: [] },
      }),
    ]);
    expect(state.stats.modelCalls).toBe(2);
    expect(state.stats.toolCalls).toBe(1);
    expect(state.stats.lastModelError).toBe('rate-limit: throttled after retries');
  });

  it('folds transition.evaluated into lastOutcome', () => {
    const state = replay([
      initialized(),
      makeEvent({
        type: 'transition.evaluated',
        projectId: 'proj_test',
        payload: { outcome: 'ADVANCE', to: 'specification', reviseStage: null, reason: 'brief approved', evidence: ['approved v1'] },
      }),
    ]);
    expect(state.lastOutcome).toEqual({
      at: state.lastActivityAt,
      outcome: 'ADVANCE',
      to: 'specification',
      reviseStage: null,
      reason: 'brief approved',
      evidence: ['approved v1'],
    });
  });

  it('tallies verification.completed results', () => {
    const state = replay([initialized(), verificationCompleted(false)]);
    expect(state.verification).toEqual({
      passed: false,
      lastRunAt: state.lastActivityAt,
      passedCount: 1,
      failedCount: 1,
      unevaluatedCount: 1,
    });
  });

  it('folds workflow.paused and workflow.resumed', () => {
    const paused = replay([
      initialized(),
      makeEvent({
        type: 'workflow.paused',
        projectId: 'proj_test',
        payload: { by: 'user', reason: 'reviewing', question: 'Keep going?', options: ['yes', 'no'] },
      }),
    ]);
    expect(paused.paused).toEqual({
      by: 'user',
      reason: 'reviewing',
      question: 'Keep going?',
      options: ['yes', 'no'],
      at: paused.lastActivityAt,
    });

    const resumed = replay([
      initialized(),
      makeEvent({ type: 'workflow.paused', projectId: 'proj_test', payload: { by: 'user', reason: 'reviewing', question: null, options: [] } }),
      makeEvent({ type: 'workflow.resumed', projectId: 'proj_test', payload: { by: 'user', note: 'back' } }),
    ]);
    expect(resumed.paused).toBeNull();
  });
});

describe('reducer: pause precedence', () => {
  it('mode.entered clears a policy pause but keeps a user pause', () => {
    const cleared = replay([
      initialized(),
      makeEvent({ type: 'workflow.paused', projectId: 'proj_test', payload: { by: 'policy', reason: 'checkpoint', question: null, options: [] } }),
      modeEntered('specification', 'discovery', 'auto', 'resumed work'),
    ]);
    expect(cleared.paused).toBeNull();

    const kept = replay([
      initialized(),
      makeEvent({ type: 'workflow.paused', projectId: 'proj_test', payload: { by: 'user', reason: 'lunch', question: null, options: [] } }),
      modeEntered('specification', 'discovery', 'user', 'forced while paused'),
    ]);
    expect(kept.paused).not.toBeNull();
    expect(kept.paused!.by).toBe('user');
  });

  it('budget.exhausted pauses by policy with a naming reason', () => {
    const state = replay([
      initialized(),
      makeEvent({ type: 'budget.exhausted', projectId: 'proj_test', payload: { budget: 'modelCalls', limit: 25, detail: 'too many calls' } }),
    ]);
    expect(state.paused).toEqual({
      by: 'policy',
      reason: 'budget exhausted: modelCalls (limit 25)',
      question: null,
      options: [],
      at: state.lastActivityAt,
    });
  });

  it('run.interrupted pauses by user with the hint', () => {
    const state = replay([
      initialized(),
      makeEvent({ type: 'run.interrupted', projectId: 'proj_test', payload: { phase: 'implementation', hint: 'Ctrl-C during patch' } }),
    ]);
    expect(state.paused).toEqual({
      by: 'user',
      reason: 'interrupted: Ctrl-C during patch',
      question: null,
      options: [],
      at: state.lastActivityAt,
    });
  });

  it('budget.exhausted and run.interrupted never overwrite an existing pause', () => {
    const state = replay([
      initialized(),
      makeEvent({ type: 'workflow.paused', projectId: 'proj_test', payload: { by: 'user', reason: 'reviewing first', question: null, options: [] } }),
      makeEvent({ type: 'budget.exhausted', projectId: 'proj_test', payload: { budget: 'toolCalls', limit: 5, detail: '' } }),
      makeEvent({ type: 'run.interrupted', projectId: 'proj_test', payload: { phase: 'verification', hint: 'signal' } }),
    ]);
    expect(state.paused!.reason).toBe('reviewing first');
    expect(state.paused!.by).toBe('user');
  });
});

describe('reducer: revision staleness chains', () => {
  it('revising the approved PRD stales every transitive dependent except the decision log', () => {
    const state = replay([...fullPipelineApproved(), artifactRevised('art-prd', 'prd', 1, 'requirements changed')]);

    expect(state.artifacts['art-prd']!.status).toBe('draft');
    expect(state.artifacts['art-prd']!.currentVersion).toBe(2);
    expect(state.artifacts['art-prd']!.approvedAt).toBeNull();
    expect(state.artifacts['art-prd']!.revisions).toEqual([
      { version: 2, at: state.lastActivityAt, reason: 'requirements changed' },
    ]);

    expect(state.artifacts['art-trd']!.status).toBe('stale');
    expect(state.artifacts['art-plan']!.status).toBe('stale');
    expect(state.artifacts['art-vr']!.status).toBe('stale');
    expect(state.artifacts['art-dr']!.status).toBe('stale');
    // The decision log depends only on the brief; the brief itself is upstream.
    expect(state.artifacts['art-log']!.status).toBe('draft');
    expect(state.artifacts['art-brief']!.status).toBe('approved');

    expect(state.staleArtifacts).toEqual(['art-trd', 'art-plan', 'art-vr', 'art-dr']);
  });

  it('revising the opportunity brief also stales the decision log', () => {
    const state = replay([...fullPipelineApproved(), artifactRevised('art-brief', 'opportunity-brief', 1, 'problem reframed')]);
    expect(state.artifacts['art-log']!.status).toBe('stale');
    expect(state.staleArtifacts).toContain('art-log');
  });

  it('does not re-mark already stale artifacts and drops the regenerated one from staleArtifacts', () => {
    const state = replay([
      ...fullPipelineApproved(),
      artifactRevised('art-prd', 'prd', 1, 'first change'),
      artifactRevised('art-trd', 'trd', 1, 'regenerate trd'),
    ]);
    // The trd revision regenerated the trd itself; plan/vr/dr remain stale once each.
    expect(state.staleArtifacts).toEqual(['art-plan', 'art-vr', 'art-dr']);
    expect(state.artifacts['art-trd']!.status).toBe('draft');
  });

  it('artifact.invalidated marks listed artifacts stale and is idempotent', () => {
    const invalidate = (ids: string[]) =>
      makeEvent({
        type: 'artifact.invalidated',
        projectId: 'proj_test',
        payload: { artifactIds: ids, cause: { kind: 'prd', artifactId: 'art-prd', version: 2 }, reason: 'upstream revision' },
      });
    const state = replay([...fullPipelineApproved(), invalidate(['art-trd', 'art-plan']), invalidate(['art-plan', 'art-vr'])]);
    expect(state.staleArtifacts).toEqual(['art-trd', 'art-plan', 'art-vr']);
    expect(state.artifacts['art-dr']!.status).toBe('draft');
    // Unknown ids are ignored rather than corrupting the projection.
    const withGhost = replay([...fullPipelineApproved(), invalidate(['ghost'])]);
    expect(withGhost.staleArtifacts).toEqual([]);
  });
});

describe('reducer: replay integrity', () => {
  it('replays deterministically: folding twice yields identical state', () => {
    const events = [...fullPipelineApproved(), artifactRevised('art-trd', 'trd', 1, 'design fix')];
    expect(replay(events)).toEqual(replay(events));
  });

  it('incremental applyEvent folds equal a full replay', () => {
    const events = fullPipelineApproved();
    let incremental = createEmptyProjectState();
    for (const event of events) {
      incremental = applyEvent(incremental, event);
    }
    expect(incremental).toEqual(replay(events));
  });

  it('rejects an empty event log', () => {
    expect(() => replayProject([])).toThrow(/empty event log/);
  });

  it('rejects a log whose first event is not project.initialized', () => {
    resetEventFactories();
    const events = [
      makeEvent({ type: 'session.started', projectId: 'proj_test', payload: { sessionId: 's1', interactive: true }, sequence: 1 }),
    ];
    const error = (() => {
      try {
        replayProject(events);
      } catch (thrown) {
        return thrown as Error;
      }
      return null;
    })();
    expect(error).toBeInstanceOf(EventFoldError);
    expect(error!.message).toContain('session.started');
    expect(error!.message).toContain('sequence 1');
  });

  it('rejects a sequence gap, naming expected and found', () => {
    resetEventFactories();
    const events = [
      initialized(),
      makeEvent({ type: 'session.started', projectId: 'proj_test', payload: { sessionId: 's1', interactive: true }, sequence: 3 }),
    ];
    expect(() => replayProject(events)).toThrow(/found sequence 3 but expected 2/);
  });

  it('rejects a duplicate sequence, naming expected and found', () => {
    resetEventFactories();
    const events = [
      initialized(),
      makeEvent({ type: 'session.started', projectId: 'proj_test', payload: { sessionId: 's1', interactive: true }, sequence: 1 }),
    ];
    expect(() => replayProject(events)).toThrow(/found sequence 1 but expected 2/);
  });

  it('rejects events from a different project', () => {
    resetEventFactories();
    const events = [
      initialized(),
      makeEvent({ type: 'session.started', projectId: 'proj_other', payload: { sessionId: 's1', interactive: true } }),
    ];
    expect(() => replayProject(events)).toThrow(/belongs to project "proj_other"(.|\n)*but this log belongs to project "proj_test"/);
  });

  it('throws UnknownEventTypeError for an event type with no fold', () => {
    const rogue = { type: 'transport.beamed', sequence: 2, schemaVersion: 1 } as unknown as AnyRelayEvent;
    expect(() => applyEvent(replay([initialized()]), rogue)).toThrow(UnknownEventTypeError);
  });

  it('throws IncompatibleEventVersionError for a mismatched schema version', () => {
    const rogue = makeEvent({ type: 'session.started', projectId: 'proj_test', payload: { sessionId: 's1', interactive: true } });
    const future = { ...rogue, schemaVersion: 99 } as AnyRelayEvent;
    expect(() => applyEvent(replay([initialized()]), future)).toThrow(/schema version 99/);
  });
});
