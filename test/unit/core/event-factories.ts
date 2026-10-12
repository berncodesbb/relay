/**
 * Deterministic event factories shared by core and policy unit tests.
 *
 * Sequences and timestamps derive from a module-local counter over a fixed
 * base instant, so replayed logs are reproducible. Tests that build raw or
 * corrupt logs can override sequence/projectId/timestamp per event.
 */

import type { ArtifactKind } from '../../../src/core/artifacts.js';
import type { AnyRelayEvent, EventAppendInput, RelayEventType } from '../../../src/core/events.js';
import { EVENT_PAYLOAD_VERSIONS } from '../../../src/core/events.js';
import type { WorkflowMode } from '../../../src/core/modes.js';
import type { ProjectState } from '../../../src/core/project-state.js';
import { replayProject } from '../../../src/core/reducer.js';

const BASE_MS = Date.parse('2026-10-12T00:00:00.000Z');

let counter = 0;

/** Reset the sequence/timestamp counter for fully deterministic assertions. */
export function resetEventFactories(): void {
  counter = 0;
}

export type FactoryInput<T extends RelayEventType> = EventAppendInput<T> & {
  readonly sequence?: number;
};

export function makeEvent<T extends RelayEventType>(input: FactoryInput<T>): AnyRelayEvent {
  counter += 1;
  const sequence = input.sequence ?? counter;
  return {
    id: `evt_${counter}`,
    projectId: input.projectId,
    sequence,
    sessionId: input.sessionId ?? null,
    type: input.type,
    schemaVersion: EVENT_PAYLOAD_VERSIONS[input.type],
    timestamp: input.timestamp ?? new Date(BASE_MS + counter).toISOString(),
    correlationId: `corr_${counter}`,
    causationId: input.causationId ?? null,
    payload: input.payload,
  } as AnyRelayEvent;
}

export function initialized(goal = 'Ship a workflow CLI that preserves product intent', autonomy = 'workflow'): AnyRelayEvent {
  return makeEvent({
    type: 'project.initialized',
    projectId: 'proj_test',
    payload: { goal, workspaceRoot: '/tmp/relay-workspace', initialAutonomy: autonomy },
  });
}

export function artifactCreated(kind: ArtifactKind, artifactId: string, title = `Test ${kind}`): AnyRelayEvent {
  return makeEvent({
    type: 'artifact.created',
    projectId: 'proj_test',
    payload: {
      artifactId,
      kind,
      title,
      version: 1,
      path: `.relay/artifacts/${kind}-${artifactId}.md`,
      summary: 'generated in test',
    },
  });
}

export function artifactApproved(artifactId: string, kind: ArtifactKind, version = 1): AnyRelayEvent {
  return makeEvent({
    type: 'artifact.approved',
    projectId: 'proj_test',
    payload: { artifactId, kind, version, approvedBy: 'user' },
  });
}

export function artifactRevised(artifactId: string, kind: ArtifactKind, fromVersion: number, reason = 'test revision'): AnyRelayEvent {
  return makeEvent({
    type: 'artifact.revised',
    projectId: 'proj_test',
    payload: {
      artifactId,
      kind,
      fromVersion,
      toVersion: fromVersion + 1,
      path: `.relay/artifacts/${kind}-${artifactId}.md`,
      reason,
    },
  });
}

export function modeEntered(
  to: WorkflowMode,
  from: WorkflowMode | null,
  source: 'auto' | 'user' = 'auto',
  reason = `entered ${to}`,
): AnyRelayEvent {
  return makeEvent({
    type: 'mode.entered',
    projectId: 'proj_test',
    payload: { from, to, reason, source },
  });
}

export function verificationCompleted(passed: boolean, reportArtifactId: string | null = 'art-vr'): AnyRelayEvent {
  return makeEvent({
    type: 'verification.completed',
    projectId: 'proj_test',
    payload: {
      passed,
      results: [
        { criterion: 'criterion A', status: passed ? 'pass' : 'fail', evidence: 'evidence A' },
        { criterion: 'criterion B', status: 'pass', evidence: 'evidence B' },
        { criterion: 'criterion C', status: 'unevaluated', evidence: '' },
      ],
      reportArtifactId,
    },
  });
}

/**
 * Replay factory-built events with canonical 1..n sequences. Factory helpers
 * draw sequence numbers from a shared counter, so any log assembled from them
 * must be renumbered before `replayProject` sees it.
 */
export function replay(events: readonly AnyRelayEvent[]): ProjectState {
  return replayProject(events.map((event, index) => ({ ...event, sequence: index + 1 })));
}

/**
 * A full approved pipeline from initialization through the delivery report:
 * every contractual artifact created and approved, every mode entered in
 * order, and a passing verification run recorded before delivery.
 */
export function fullPipelineApproved(): AnyRelayEvent[] {
  return [
    initialized(),
    artifactCreated('opportunity-brief', 'art-brief'),
    artifactApproved('art-brief', 'opportunity-brief'),
    modeEntered('specification', 'discovery', 'auto', 'brief approved'),
    artifactCreated('prd', 'art-prd'),
    artifactApproved('art-prd', 'prd'),
    modeEntered('technical-design', 'specification', 'auto', 'prd approved'),
    artifactCreated('trd', 'art-trd'),
    artifactApproved('art-trd', 'trd'),
    modeEntered('planning', 'technical-design', 'auto', 'trd approved'),
    artifactCreated('execution-plan', 'art-plan'),
    artifactApproved('art-plan', 'execution-plan'),
    modeEntered('implementation', 'planning', 'auto', 'plan approved'),
    artifactCreated('decision-log', 'art-log'),
    modeEntered('verification', 'implementation', 'auto', 'implementation complete'),
    artifactCreated('verification-report', 'art-vr'),
    verificationCompleted(true),
    artifactApproved('art-vr', 'verification-report'),
    modeEntered('delivery', 'verification', 'auto', 'verification approved'),
    artifactCreated('delivery-report', 'art-dr'),
  ];
}
