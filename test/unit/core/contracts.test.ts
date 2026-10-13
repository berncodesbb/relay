import { describe, expect, it } from 'vitest';
import {
  ARTIFACT_DEPENDENCIES,
  CONTRACTUAL_ARTIFACT_KINDS,
  downstreamArtifactKinds,
  isContractualArtifactKind,
} from '../../../src/core/artifacts.js';
import {
  IncompatibleEventVersionError,
  InvalidEventPayloadError,
  UnknownEventTypeError,
  validateStoredEvent,
  EVENT_PAYLOAD_VERSIONS,
  type StoredEventEnvelope,
} from '../../../src/core/events.js';
import { intentProposalSchema, isIntentAmbiguous, parseSlashCommand } from '../../../src/core/intents.js';
import { MODE_CONTRACTS, parseAutonomyBound, WORKFLOW_MODES } from '../../../src/core/modes.js';

describe('contract smoke', () => {
  it('modes and autonomy parse', () => {
    expect(MODE_CONTRACTS.discovery!.producedArtifact).toBe('opportunity-brief');
    expect(parseAutonomyBound('until:planning')).toBe('until:planning');
  });

  it('validates a stored event', () => {
    const event = validateStoredEvent({
      id: 'e1', projectId: 'p1', sequence: 1, sessionId: null,
      type: 'project.initialized', schemaVersion: 1, timestamp: '2026-10-12T00:00:00.000Z',
      correlationId: 'c1', causationId: null,
      payload: { goal: 'g', workspaceRoot: '/tmp', initialAutonomy: 'workflow' },
    });
    expect(event.type).toBe('project.initialized');
  });
});

describe('core contracts', () => {
  it('accepts model events with and without the optional role field', () => {
    const envelope = (payload: unknown, sequence: number) => ({
      id: `e${sequence}`, projectId: 'p1', sequence, sessionId: null,
      type: 'model.requested' as const, schemaVersion: 1, timestamp: '2026-10-13T00:00:00.000Z',
      correlationId: 'c1', causationId: null, payload,
    });
    // Pre-role payload: no role key at all — must keep validating (legacy logs).
    const legacy = validateStoredEvent(envelope({
      provider: 'anthropic', model: 'claude-sonnet-5-5', purpose: 'mode:discovery', mode: 'discovery', attempt: 1,
    }, 1));
    expect((legacy.payload as Record<string, unknown>)['role']).toBeUndefined();
    // Role-aware payload: optional field validates and round-trips.
    const roled = validateStoredEvent(envelope({
      provider: 'anthropic', model: 'claude-sonnet-5-5', purpose: 'mode:discovery', mode: 'discovery', attempt: 1,
      role: 'reasoning',
    }, 2));
    expect((roled.payload as Record<string, unknown>)['role']).toBe('reasoning');
    // Role must be a non-empty bounded string when present.
    expect(() => validateStoredEvent(envelope({
      provider: 'anthropic', model: 'claude-sonnet-5-5', purpose: 'mode:discovery', mode: 'discovery', attempt: 1,
      role: '',
    }, 3))).toThrow(/role/);
  });

  it('keeps model event payload schema versions at 1 (additive role field)', () => {
    expect(EVENT_PAYLOAD_VERSIONS['model.requested']).toBe(1);
    expect(EVENT_PAYLOAD_VERSIONS['model.responded']).toBe(1);
    expect(EVENT_PAYLOAD_VERSIONS['model.failed']).toBe(1);
  });

  it('orders the seven workflow modes', () => {
    expect(WORKFLOW_MODES).toEqual([
      'discovery',
      'specification',
      'technical-design',
      'planning',
      'implementation',
      'verification',
      'delivery',
    ]);
  });

  it('parses autonomy bounds and rejects garbage', () => {
    expect(parseAutonomyBound('off')).toBe('off');
    expect(parseAutonomyBound('next-checkpoint')).toBe('next-checkpoint');
    expect(parseAutonomyBound('workflow')).toBe('workflow');
    expect(parseAutonomyBound('until:delivery')).toBe('until:delivery');
    expect(parseAutonomyBound('until:nowhere')).toBeNull();
    expect(parseAutonomyBound(42)).toBeNull();
  });

  it('derives the artifact dependency graph and downstream sets', () => {
    expect(ARTIFACT_DEPENDENCIES['execution-plan']).toEqual(['prd', 'trd']);
    expect(downstreamArtifactKinds('prd').sort()).toEqual(
      ['trd', 'execution-plan', 'verification-report', 'delivery-report'].sort(),
    );
    expect(downstreamArtifactKinds('opportunity-brief').sort()).toEqual(
      ['decision-log', 'prd', 'trd', 'execution-plan', 'verification-report', 'delivery-report'].sort(),
    );
    expect(downstreamArtifactKinds('delivery-report')).toEqual([]);
  });

  it('classifies contractual artifact kinds', () => {
    expect(isContractualArtifactKind('prd')).toBe(true);
    expect(isContractualArtifactKind('decision-log')).toBe(false);
    expect(isContractualArtifactKind('delivery-report')).toBe(false);
    expect(CONTRACTUAL_ARTIFACT_KINDS).toHaveLength(5);
  });

  it('rejects unknown event types with actionable diagnostics', () => {
    const envelope = {
      id: 'e1', projectId: 'p1', sequence: 1, sessionId: null,
      type: 'magic.happened', schemaVersion: 1, timestamp: '2026-10-12T00:00:00.000Z',
      correlationId: 'c1', causationId: null, payload: {},
    } as unknown as StoredEventEnvelope;
    expect(() => validateStoredEvent(envelope)).toThrow(UnknownEventTypeError);
  });

  it('rejects incompatible event versions', () => {
    const envelope: StoredEventEnvelope = {
      id: 'e1', projectId: 'p1', sequence: 1, sessionId: null,
      type: 'project.initialized', schemaVersion: 7, timestamp: '2026-10-12T00:00:00.000Z',
      correlationId: 'c1', causationId: null,
      payload: { goal: 'g', workspaceRoot: '/tmp', initialAutonomy: 'workflow' },
    };
    expect(() => validateStoredEvent(envelope)).toThrow(IncompatibleEventVersionError);
  });

  it('rejects invalid payloads with field-level detail', () => {
    const envelope: StoredEventEnvelope = {
      id: 'e1', projectId: 'p1', sequence: 4, sessionId: null,
      type: 'project.initialized', schemaVersion: 1, timestamp: '2026-10-12T00:00:00.000Z',
      correlationId: 'c1', causationId: null,
      payload: { goal: '', workspaceRoot: '/tmp' },
    };
    expect(() => validateStoredEvent(envelope)).toThrow(InvalidEventPayloadError);
    expect(() => validateStoredEvent(envelope)).toThrow(/sequence 4/);
  });

  it('parses slash commands and rejects unknown ones', () => {
    expect(parseSlashCommand('/status')).toEqual({ command: 'status' });
    expect(parseSlashCommand('/mode planning')).toEqual({ command: 'mode', mode: 'planning' });
    expect(parseSlashCommand('/auto until:verification')).toEqual({ command: 'auto', bound: 'until:verification' });
    expect(parseSlashCommand('/revise prd tighten the scope')).toEqual({
      command: 'revise',
      artifactKind: 'prd',
      reason: 'tighten the scope',
    });
    expect(parseSlashCommand('/nope')).toBeNull();
    expect(parseSlashCommand('plain text')).toBeNull();
  });

  it('validates intent proposals and flags ambiguity', () => {
    const proposal = intentProposalSchema.parse({
      kind: 'change-mode',
      confidence: 0.5,
      rationale: 'maybe switch modes',
      mode: 'planning',
    });
    expect(isIntentAmbiguous(proposal)).toBe(true); // consequential intent below 0.8

    const clear = intentProposalSchema.parse({
      kind: 'request-status',
      confidence: 0.95,
      rationale: 'user asked for status',
    });
    expect(isIntentAmbiguous(clear)).toBe(false);

    expect(intentProposalSchema.safeParse({ kind: 'advance', confidence: 2, rationale: 'x' }).success).toBe(false);
  });
});
