/**
 * Core event contracts.
 *
 * Every durable workflow change is an event in an append-only log. The event
 * union, payload schemas, and schema versions defined here are shared
 * contracts: storage persists them verbatim, the reducer replays them, and the
 * controller only ever appends. Unknown types or incompatible versions must be
 * rejected with actionable diagnostics during replay.
 *
 * Events never contain secrets, API keys, or authorization headers.
 */

import { z } from 'zod';
import { ARTIFACT_KINDS } from './artifacts.js';
import { intentProposalSchema, type AppliedIntent, type IntentSource } from './intents.js';
import { WORKFLOW_MODES, type AutonomyBound, type WorkflowMode } from './modes.js';
import type { OutcomeKind } from './outcomes.js';

/** Version of the event envelope itself. */
export const EVENT_ENVELOPE_VERSION = 1;

export type TokenUsage = {
  readonly inputTokens: number;
  readonly outputTokens: number;
};

export type ModelErrorKind =
  | 'rate-limit'
  | 'server'
  | 'network'
  | 'timeout'
  | 'canceled'
  | 'auth'
  | 'invalid-response'
  | 'unknown';

const isoTimestamp = z.string().min(1);
const mode = z.enum(WORKFLOW_MODES);
const artifactKind = z.enum(ARTIFACT_KINDS);

// ---------------------------------------------------------------------------
// Payload schemas
// ---------------------------------------------------------------------------

export const projectInitializedSchema = z.object({
  goal: z.string().min(1).max(20000),
  workspaceRoot: z.string().min(1),
  initialAutonomy: z.string(),
});

export type ProjectInitializedPayload = z.infer<typeof projectInitializedSchema>;

export const sessionStartedSchema = z.object({
  sessionId: z.string().min(1),
  interactive: z.boolean(),
});

export type SessionStartedPayload = z.infer<typeof sessionStartedSchema>;

export const intentProposedSchema = z.object({
  source: z.enum(['natural-language', 'command']),
  raw: z.string().max(20000),
  proposal: intentProposalSchema,
});

export type IntentProposedPayload = z.infer<typeof intentProposedSchema>;

export const intentAppliedSchema = z.object({
  kind: z.string().min(1),
  source: z.enum(['natural-language', 'command']),
  confidence: z.number().min(0).max(1),
  mode: mode.nullable(),
  autonomy: z.string().nullable(),
  artifactKind: artifactKind.nullable(),
  overridden: z.boolean(),
});

export type IntentAppliedPayload = z.infer<typeof intentAppliedSchema>;

export const modeEnteredSchema = z.object({
  from: mode.nullable(),
  to: mode,
  reason: z.string().min(1).max(2000),
  source: z.enum(['auto', 'user']),
});

export type ModeEnteredPayload = z.infer<typeof modeEnteredSchema>;

export const modeTransitionRejectedSchema = z.object({
  from: mode,
  to: mode,
  reason: z.string().min(1).max(2000),
});

export type ModeTransitionRejectedPayload = z.infer<typeof modeTransitionRejectedSchema>;

export const autonomyChangedSchema = z.object({
  from: z.string(),
  to: z.string(),
  source: z.enum(['auto', 'user', 'intent']),
});

export type AutonomyChangedPayload = z.infer<typeof autonomyChangedSchema>;

export const artifactCreatedSchema = z.object({
  artifactId: z.string().min(1),
  kind: artifactKind,
  title: z.string().min(1).max(300),
  version: z.literal(1),
  /** Path relative to the workspace root. */
  path: z.string().min(1),
  summary: z.string().max(4000),
});

export type ArtifactCreatedPayload = z.infer<typeof artifactCreatedSchema>;

export const artifactRevisedSchema = z.object({
  artifactId: z.string().min(1),
  kind: artifactKind,
  fromVersion: z.number().int().min(1),
  toVersion: z.number().int().min(2),
  path: z.string().min(1),
  reason: z.string().min(1).max(2000),
});

export type ArtifactRevisedPayload = z.infer<typeof artifactRevisedSchema>;

export const artifactApprovalRequestedSchema = z.object({
  artifactId: z.string().min(1),
  kind: artifactKind,
  version: z.number().int().min(1),
  reason: z.string().min(1).max(2000),
});

export type ArtifactApprovalRequestedPayload = z.infer<typeof artifactApprovalRequestedSchema>;

export const artifactApprovedSchema = z.object({
  artifactId: z.string().min(1),
  kind: artifactKind,
  version: z.number().int().min(1),
  approvedBy: z.literal('user'),
});

export type ArtifactApprovedPayload = z.infer<typeof artifactApprovedSchema>;

export const artifactInvalidatedSchema = z.object({
  artifactIds: z.array(z.string().min(1)).min(1),
  cause: z.object({
    kind: artifactKind,
    artifactId: z.string().min(1),
    version: z.number().int().min(1),
  }),
  reason: z.string().min(1).max(2000),
});

export type ArtifactInvalidatedPayload = z.infer<typeof artifactInvalidatedSchema>;

export const artifactInspectedSchema = z.object({
  artifactId: z.string().min(1),
  via: z.enum(['user', 'model']),
});

export type ArtifactInspectedPayload = z.infer<typeof artifactInspectedSchema>;

export const transitionEvaluatedSchema = z.object({
  outcome: z.enum(['CONTINUE', 'ADVANCE', 'NEEDS_USER', 'REVISE']),
  to: mode.nullable(),
  reviseStage: mode.nullable(),
  reason: z.string().min(1).max(2000),
  evidence: z.array(z.string().max(1000)).max(20),
});

export type TransitionEvaluatedPayload = z.infer<typeof transitionEvaluatedSchema>;

export const modelRequestedSchema = z.object({
  provider: z.string().min(1),
  model: z.string().min(1),
  purpose: z.string().min(1).max(200),
  mode: mode.nullable(),
  attempt: z.number().int().min(1),
  /**
   * Semantic model-role name selected by routing, when the route came from a
   * role reference. Optional and absent for legacy inline routes and pre-role
   * event logs, which must keep validating and replaying unchanged.
   */
  role: z.string().min(1).max(100).optional(),
});

export type ModelRequestedPayload = z.infer<typeof modelRequestedSchema>;

export const modelRespondedSchema = z.object({
  provider: z.string().min(1),
  model: z.string().min(1),
  purpose: z.string().min(1).max(200),
  role: z.string().min(1).max(100).optional(),
  usage: z.object({ inputTokens: z.number().int().min(0), outputTokens: z.number().int().min(0) }),
  toolCalls: z.number().int().min(0),
  durationMs: z.number().int().min(0),
});

export type ModelRespondedPayload = z.infer<typeof modelRespondedSchema>;

export const modelFailedSchema = z.object({
  provider: z.string().min(1),
  model: z.string().min(1),
  purpose: z.string().min(1).max(200),
  role: z.string().min(1).max(100).optional(),
  errorKind: z.enum([
    'rate-limit',
    'server',
    'network',
    'timeout',
    'canceled',
    'auth',
    'invalid-response',
    'unknown',
  ]),
  /** Redacted, header-free error summary. */
  message: z.string().max(2000),
  attempts: z.number().int().min(1),
  durationMs: z.number().int().min(0),
});

export type ModelFailedPayload = z.infer<typeof modelFailedSchema>;

export const toolExecutedSchema = z.object({
  tool: z.string().min(1),
  capability: z.enum(['read', 'write', 'execute']),
  mode: mode.nullable(),
  outcome: z.enum(['ok', 'error']),
  durationMs: z.number().int().min(0),
  /** Redacted and length-bounded summary of what happened. */
  summary: z.string().max(2000),
  affectedFiles: z.array(z.string().max(1000)).max(500),
});

export type ToolExecutedPayload = z.infer<typeof toolExecutedSchema>;

export const toolDeniedSchema = z.object({
  tool: z.string().min(1),
  capability: z.enum(['read', 'write', 'execute']),
  mode: mode.nullable(),
  reason: z.string().min(1).max(2000),
});

export type ToolDeniedPayload = z.infer<typeof toolDeniedSchema>;

export const verificationCompletedSchema = z.object({
  passed: z.boolean(),
  results: z
    .array(
      z.object({
        criterion: z.string().min(1).max(2000),
        status: z.enum(['pass', 'fail', 'unevaluated']),
        evidence: z.string().max(4000),
      }),
    )
    .max(200),
  reportArtifactId: z.string().min(1).nullable(),
});

export type VerificationCompletedPayload = z.infer<typeof verificationCompletedSchema>;

export const workflowPausedSchema = z.object({
  by: z.enum(['user', 'policy']),
  reason: z.string().min(1).max(2000),
  question: z.string().max(2000).nullable(),
  options: z.array(z.string().max(300)).max(6),
});

export type WorkflowPausedPayload = z.infer<typeof workflowPausedSchema>;

export const workflowResumedSchema = z.object({
  by: z.literal('user'),
  note: z.string().max(2000),
});

export type WorkflowResumedPayload = z.infer<typeof workflowResumedSchema>;

export const budgetExhaustedSchema = z.object({
  budget: z.string().min(1).max(100),
  limit: z.number().int().min(0),
  detail: z.string().max(2000),
});

export type BudgetExhaustedPayload = z.infer<typeof budgetExhaustedSchema>;

export const runInterruptedSchema = z.object({
  phase: z.string().max(200),
  hint: z.string().max(2000),
});

export type RunInterruptedPayload = z.infer<typeof runInterruptedSchema>;

// ---------------------------------------------------------------------------
// Event union
// ---------------------------------------------------------------------------

export const RELAY_EVENT_TYPES = [
  'project.initialized',
  'session.started',
  'intent.proposed',
  'intent.applied',
  'mode.entered',
  'mode.transition.rejected',
  'autonomy.changed',
  'artifact.created',
  'artifact.revised',
  'artifact.approval.requested',
  'artifact.approved',
  'artifact.invalidated',
  'artifact.inspected',
  'transition.evaluated',
  'model.requested',
  'model.responded',
  'model.failed',
  'tool.executed',
  'tool.denied',
  'verification.completed',
  'workflow.paused',
  'workflow.resumed',
  'budget.exhausted',
  'run.interrupted',
] as const;

export type RelayEventType = (typeof RELAY_EVENT_TYPES)[number];

/** Per-type payload schema versions. Bump when a payload shape changes incompatibly. */
export const EVENT_PAYLOAD_VERSIONS: Readonly<Record<RelayEventType, number>> = {
  'project.initialized': 1,
  'session.started': 1,
  'intent.proposed': 1,
  'intent.applied': 1,
  'mode.entered': 1,
  'mode.transition.rejected': 1,
  'autonomy.changed': 1,
  'artifact.created': 1,
  'artifact.revised': 1,
  'artifact.approval.requested': 1,
  'artifact.approved': 1,
  'artifact.invalidated': 1,
  'artifact.inspected': 1,
  'transition.evaluated': 1,
  'model.requested': 1,
  'model.responded': 1,
  'model.failed': 1,
  'tool.executed': 1,
  'tool.denied': 1,
  'verification.completed': 1,
  'workflow.paused': 1,
  'workflow.resumed': 1,
  'budget.exhausted': 1,
  'run.interrupted': 1,
};

export const eventPayloadSchemas = {
  'project.initialized': projectInitializedSchema,
  'session.started': sessionStartedSchema,
  'intent.proposed': intentProposedSchema,
  'intent.applied': intentAppliedSchema,
  'mode.entered': modeEnteredSchema,
  'mode.transition.rejected': modeTransitionRejectedSchema,
  'autonomy.changed': autonomyChangedSchema,
  'artifact.created': artifactCreatedSchema,
  'artifact.revised': artifactRevisedSchema,
  'artifact.approval.requested': artifactApprovalRequestedSchema,
  'artifact.approved': artifactApprovedSchema,
  'artifact.invalidated': artifactInvalidatedSchema,
  'artifact.inspected': artifactInspectedSchema,
  'transition.evaluated': transitionEvaluatedSchema,
  'model.requested': modelRequestedSchema,
  'model.responded': modelRespondedSchema,
  'model.failed': modelFailedSchema,
  'tool.executed': toolExecutedSchema,
  'tool.denied': toolDeniedSchema,
  'verification.completed': verificationCompletedSchema,
  'workflow.paused': workflowPausedSchema,
  'workflow.resumed': workflowResumedSchema,
  'budget.exhausted': budgetExhaustedSchema,
  'run.interrupted': runInterruptedSchema,
} as const;

/** Raw event as persisted/read from storage (payload still JSON-encoded or unknown). */
export type StoredEventEnvelope = {
  readonly id: string;
  readonly projectId: string;
  readonly sequence: number;
  readonly sessionId: string | null;
  readonly type: RelayEventType;
  readonly schemaVersion: number;
  readonly timestamp: string;
  readonly correlationId: string;
  readonly causationId: string | null;
  readonly payload: unknown;
};

export type RelayEvent<T extends RelayEventType = RelayEventType> = {
  readonly id: string;
  readonly projectId: string;
  readonly sequence: number;
  readonly sessionId: string | null;
  readonly type: T;
  readonly schemaVersion: number;
  readonly timestamp: string;
  readonly correlationId: string;
  readonly causationId: string | null;
  readonly payload: T extends RelayEventType ? z.infer<(typeof eventPayloadSchemas)[T]> : never;
};

export type AnyRelayEvent =
  | { readonly type: 'project.initialized'; payload: ProjectInitializedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'session.started'; payload: SessionStartedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'intent.proposed'; payload: IntentProposedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'intent.applied'; payload: IntentAppliedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'mode.entered'; payload: ModeEnteredPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'mode.transition.rejected'; payload: ModeTransitionRejectedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'autonomy.changed'; payload: AutonomyChangedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'artifact.created'; payload: ArtifactCreatedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'artifact.revised'; payload: ArtifactRevisedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'artifact.approval.requested'; payload: ArtifactApprovalRequestedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'artifact.approved'; payload: ArtifactApprovedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'artifact.invalidated'; payload: ArtifactInvalidatedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'artifact.inspected'; payload: ArtifactInspectedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'transition.evaluated'; payload: TransitionEvaluatedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'model.requested'; payload: ModelRequestedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'model.responded'; payload: ModelRespondedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'model.failed'; payload: ModelFailedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'tool.executed'; payload: ToolExecutedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'tool.denied'; payload: ToolDeniedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'verification.completed'; payload: VerificationCompletedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'workflow.paused'; payload: WorkflowPausedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'workflow.resumed'; payload: WorkflowResumedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'budget.exhausted'; payload: BudgetExhaustedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>
  | { readonly type: 'run.interrupted'; payload: RunInterruptedPayload } & Omit<StoredEventEnvelope, 'type' | 'payload'>;

export type EventRecorder = {
  /**
   * Validate and append an event. The store assigns id, sequence, timestamp,
   * correlation/causation ids when not supplied; the payload is validated
   * against the schema registered for `type`.
   */
  append(input: EventAppendInput): Promise<AnyRelayEvent>;
};

export type EventAppendInput<T extends RelayEventType = RelayEventType> = {
  readonly type: T;
  readonly projectId: string;
  readonly sessionId?: string | null;
  readonly payload: T extends RelayEventType ? z.infer<(typeof eventPayloadSchemas)[T]> : never;
  readonly timestamp?: string;
  readonly correlationId?: string;
  readonly causationId?: string | null;
};

export class UnknownEventTypeError extends Error {
  constructor(
    readonly eventType: string,
    readonly detail: string,
  ) {
    super(`Unknown event type "${eventType}": ${detail}`);
    this.name = 'UnknownEventTypeError';
  }
}

export class IncompatibleEventVersionError extends Error {
  constructor(
    readonly eventType: string,
    readonly found: number,
    readonly expected: number,
  ) {
    super(
      `Event "${eventType}" has schema version ${found}, but this Relay build understands version ${expected}. ` +
        'The project was likely written by a newer Relay. Upgrade Relay or archive this project directory.',
    );
    this.name = 'IncompatibleEventVersionError';
  }
}

export class InvalidEventPayloadError extends Error {
  constructor(
    readonly eventType: string,
    readonly sequence: number,
    readonly detail: string,
  ) {
    super(`Event "${eventType}" (sequence ${sequence}) has an invalid payload: ${detail}`);
    this.name = 'InvalidEventPayloadError';
  }
}

/** Validate a raw stored envelope into a typed event, with actionable diagnostics. */
export function validateStoredEvent(envelope: StoredEventEnvelope): AnyRelayEvent {
  const known = (RELAY_EVENT_TYPES as readonly string[]).includes(envelope.type);
  if (!known) {
    throw new UnknownEventTypeError(
      envelope.type,
      `at sequence ${envelope.sequence}. The event log may come from a different tool or a newer Relay version.`,
    );
  }
  const expectedVersion = EVENT_PAYLOAD_VERSIONS[envelope.type];
  if (envelope.schemaVersion !== expectedVersion) {
    throw new IncompatibleEventVersionError(envelope.type, envelope.schemaVersion, expectedVersion);
  }
  const schema = eventPayloadSchemas[envelope.type];
  const parsed = schema.safeParse(envelope.payload);
  if (!parsed.success) {
    throw new InvalidEventPayloadError(
      envelope.type,
      envelope.sequence,
      parsed.error.issues.map((issue) => `${issue.path.join('.') || '(root)'}: ${issue.message}`).join('; '),
    );
  }
  return { ...envelope, payload: parsed.data } as AnyRelayEvent;
}

export type { AppliedIntent, IntentSource, AutonomyBound, WorkflowMode, OutcomeKind };
