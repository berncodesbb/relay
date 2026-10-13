/**
 * The workflow controller: Relay's integration heart.
 *
 * The controller owns no workflow opinions of its own. It appends validated
 * events, recomputes state by folding the log after every append, runs the
 * agent loop per mode with policy-built permission gates and fresh budgets,
 * validates every model-produced draft against the artifact schemas, and then
 * applies the deterministic transition policy — models may propose, only
 * policy decides. Every step surfaces a {@link ControllerOutcome} plus UI
 * events for the host interface; durable truth always lives in the event log.
 */

import type { z } from 'zod';
import { isArtifactKind, isContractualArtifactKind, type ArtifactKind } from '../core/artifacts.js';
import {
  eventPayloadSchemas,
  type AnyRelayEvent,
  type EventAppendInput,
  type RelayEventType,
} from '../core/events.js';
import {
  intentProposalSchema,
  isIntentAmbiguous,
  parseSlashCommand,
  type IntentProposal,
  type ParsedCommand,
} from '../core/intents.js';
import {
  MODE_CONTRACTS,
  parseAutonomyBound,
  type AutonomyBound,
  type ToolCapability,
  type WorkflowMode,
} from '../core/modes.js';
import type { ControllerOutcome } from '../core/outcomes.js';
import { createEmptyProjectState, type ProjectState } from '../core/project-state.js';
import { newId } from '../core/ids.js';
import { earliestAffectedStage, KIND_TO_MODE, shouldAutoAdvance } from '../core/transitions.js';
import { requiresUserIntervention } from '../policies/intervention.js';
import { createPermissionGate } from '../policies/permissions.js';
import {
  assertTransitionAllowed,
  evaluatePolicyTransition,
  PolicyRejectionError,
} from '../policies/transitions.js';
import { kindToMode } from '../artifacts/invalidation.js';
import type { ArtifactMarkdownStore } from '../artifacts/markdown-store.js';
import { renderArtifactMarkdown } from '../artifacts/render.js';
import { ARTIFACT_DRAFT_SCHEMAS, parseArtifactDraft } from '../artifacts/schemas.js';
import type { ParseArtifactDraftSuccess } from '../artifacts/schemas.js';
import type { BudgetLimits } from '../config/schema.js';
import type { ProjectRepository } from '../storage/project-repository.js';
import type { SqliteEventStore } from '../storage/event-store.js';
import type { JsonSchemaObject, ModelMessage, ProviderName } from '../providers/provider.js';
import type { ToolRegistry } from '../tools/registry.js';
import type { ModelRouter } from './model-router.js';
import { RouteUnavailableError, type ResolvedRoute } from './model-router.js';
import { BudgetTracker } from './budgets.js';
import { CancellationToken } from './cancellation.js';
import { runAgentLoop, type AgentLoopEvent, type AgentLoopResult } from './agent-loop.js';
import { buildModeContext } from './context-builder.js';
import { modePrompt } from './prompts.js';
import { buildStatusView, buildWhyView, type StatusView, type WhyView } from './controller-views.js';

// ---------------------------------------------------------------------------
// Public contracts
// ---------------------------------------------------------------------------

export type ControllerUiEvent =
  | { kind: 'session'; sessionId: string }
  | { kind: 'mode'; mode: WorkflowMode; previous: WorkflowMode | null; reason: string }
  | { kind: 'delta'; text: string }
  | { kind: 'tool'; tool: string; capability: ToolCapability; phase: 'start' | 'end'; ok?: boolean; summary?: string }
  | { kind: 'artifact'; artifactId: string; artifactKind: ArtifactKind; version: number; status: 'draft' | 'approved' | 'stale' }
  | { kind: 'outcome'; outcome: ControllerOutcome }
  | { kind: 'notice'; message: string }
  | { kind: 'error'; message: string };

export type WorkflowServices = {
  readonly workspaceRoot: string;
  readonly repository: ProjectRepository;
  readonly eventStore: SqliteEventStore;
  readonly artifacts: ArtifactMarkdownStore;
  readonly router: ModelRouter;
  readonly tools: ToolRegistry;
  readonly budgets: BudgetLimits;
};

export type StepResult = { readonly outcome: ControllerOutcome; readonly state: ProjectState };

export type ControllerOptions = {
  readonly sessionId?: string;
  readonly providerOverrides?: { readonly provider?: ProviderName; readonly model?: string };
  /**
   * Invocation-wide semantic role override (`--role`): replaces the configured
   * route's chain with the named role's expanded chain. Must exist in the
   * effective model catalog; unknown roles fail with an actionable error.
   */
  readonly roleOverride?: string;
  /** Autonomy recorded at initialization; defaults to 'next-checkpoint'. */
  readonly initialAutonomy?: AutonomyBound;
  /** Aborts in-flight model steps cooperatively. */
  readonly signal?: AbortSignal;
};

/** Raised when an operation requires an initialized project and none exists. */
export class NotInitializedError extends Error {
  constructor(detail: string) {
    super(`No Relay project is initialized: ${detail}`);
    this.name = 'NotInitializedError';
  }
}

/** Raised by {@link WorkflowController.initialize} on an already-initialized workspace. */
export class ProjectAlreadyInitializedError extends Error {
  constructor(readonly projectId: string) {
    super(
      `This workspace already has a Relay project (${projectId}); continue it or use a fresh workspace.`,
    );
    this.name = 'ProjectAlreadyInitializedError';
  }
}

type EventPayloadOf<T extends RelayEventType> = z.infer<(typeof eventPayloadSchemas)[T]>;
type DraftParse = ParseArtifactDraftSuccess<ArtifactKind>;

const DEFAULT_MAX_STEPS = 50;
const INTENT_SYSTEM_PROMPT = [
  'You classify user input for a local-first development workflow CLI.',
  'Respond ONLY with a JSON object matching the provided intent-proposal schema.',
  'Choose the single best intent kind; set confidence between 0 and 1 (below 0.6 the controller will ask a focused question instead of acting).',
  'For consequential intents (revise-artifact, change-mode, set-autonomy) use confidence below 0.8 unless the request is explicit and unambiguous.',
  'Include mode/autonomy/artifactKind only when the input names them; otherwise omit those fields.',
  'When the input is ambiguous, set question to one focused clarifying question naming your top interpretation.',
  'freeText should restate the user\'s substantive content (a goal, a revision reason, an answer) without commentary.',
].join('\n');

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

export class WorkflowController {
  private readonly services: WorkflowServices;
  private readonly sessionId: string;
  private readonly providerOverrides:
    | { readonly provider?: ProviderName; readonly model?: string }
    | undefined;
  private readonly roleOverride: string | undefined;
  private readonly initialAutonomy: AutonomyBound | undefined;
  private readonly signal: AbortSignal | undefined;
  private readonly listeners = new Set<(event: ControllerUiEvent) => void>();
  private readonly pendingAppends: Promise<unknown>[] = [];
  private projectIdValue: string | null = null;
  private stateCache: ProjectState | null = null;
  private pendingUserNote: string | null = null;

  constructor(services: WorkflowServices, options?: ControllerOptions) {
    this.services = services;
    this.sessionId = options?.sessionId ?? newId('session');
    this.providerOverrides = options?.providerOverrides;
    this.roleOverride = options?.roleOverride;
    this.initialAutonomy = options?.initialAutonomy;
    this.signal = options?.signal;
    const projects = services.repository.listProjects();
    if (projects.length === 1) {
      const only = projects[0];
      if (only !== undefined) {
        const loaded = services.repository.loadProject(only.projectId);
        this.projectIdValue = loaded.projectId;
        this.stateCache = loaded.state;
      }
    } else if (projects.length > 1) {
      const ids = projects.map((project) => project.projectId).join(', ');
      throw new Error(
        `This workspace has ${projects.length} Relay projects (${ids}); the controller needs a workspace with exactly one project.`,
      );
    }
  }

  get projectId(): string | null {
    return this.projectIdValue;
  }

  get state(): ProjectState {
    if (this.stateCache === null || this.projectIdValue === null) {
      throw new NotInitializedError(
        'initialize a project (or describe your goal) before reading state.',
      );
    }
    return this.stateCache;
  }

  subscribe(listener: (event: ControllerUiEvent) => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  // -- lifecycle ---------------------------------------------------------------

  async initialize(goal: string): Promise<void> {
    if (this.projectIdValue !== null) {
      throw new ProjectAlreadyInitializedError(this.projectIdValue);
    }
    const trimmed = goal.trim();
    if (trimmed.length === 0) {
      throw new Error('initialize requires a non-empty goal describing what this project should deliver.');
    }
    const autonomy = this.initialAutonomy ?? 'next-checkpoint';
    const initialized = await this.services.repository.initializeProject({
      goal: trimmed,
      workspaceRoot: this.services.workspaceRoot,
      autonomy,
    });
    this.projectIdValue = initialized.projectId;
    this.stateCache = initialized.state;
    this.append('session.started', { sessionId: this.sessionId, interactive: false });
    await this.settle();
    this.emit({ kind: 'session', sessionId: this.sessionId });
    this.emit({
      kind: 'mode',
      mode: this.state.currentMode,
      previous: null,
      reason: 'project initialized',
    });
  }

  async handleInput(input: string): Promise<StepResult> {
    const trimmed = input.trim();
    if (trimmed.length === 0) {
      return this.finish({
        kind: 'NEEDS_USER',
        question: 'say something — describe a goal, answer a question, or use a /command',
        options: ['/status', '/why'],
        reason: 'empty input',
      });
    }

    if (trimmed.startsWith('/')) {
      const command = parseSlashCommand(trimmed);
      if (command === null) {
        return this.finish({
          kind: 'NEEDS_USER',
          question: `unknown or malformed command: ${trimmed.slice(0, 120)}`,
          options: [
            '/status',
            '/why',
            '/inspect <artifact>',
            '/mode <mode>',
            '/auto <bound>',
            '/approve',
            '/revise <kind> <reason>',
          ],
          reason: 'unrecognized slash command',
        });
      }
      if (this.projectIdValue === null) {
        return this.finish({
          kind: 'NEEDS_USER',
          question: 'initialize the project first — describe your goal in plain language or run init',
          options: ['describe your goal'],
          reason: 'command requires an initialized project',
        });
      }
      this.append('intent.proposed', {
        source: 'command',
        raw: bounded(trimmed, 20_000),
        proposal: proposalForCommand(command),
      });
      await this.settle();
      const result = await this.applyCommand(command);
      this.append('intent.applied', appliedForCommand(command));
      await this.settle();
      return result;
    }

    // Natural language: classify into a validated intent proposal first.
    const classification = await this.classifyIntent(trimmed);
    if (classification === null) {
      return this.finish({
        kind: 'NEEDS_USER',
        question: 'could not classify that input into a valid intent — rephrase it or use a /command',
        options: ['/status', '/why'],
        reason: 'intent classification produced an invalid response',
      });
    }
    if ('failure' in classification) {
      return classification.failure;
    }
    const proposal = classification.proposal;

    // An unambiguous goal on an uninitialized workspace must create the
    // project before any intent events can be appended to it.
    if (
      this.projectIdValue === null
      && proposal.kind === 'provide-goal'
      && !isIntentAmbiguous(proposal)
    ) {
      const goal = (proposal.freeText ?? '').trim() || trimmed;
      await this.initialize(goal);
    }

    this.append('intent.proposed', {
      source: 'natural-language',
      raw: bounded(trimmed, 20_000),
      proposal,
    });
    await this.settle();

    if (isIntentAmbiguous(proposal)) {
      const question = proposal.question ?? focusedQuestion(proposal, trimmed);
      const options = interpretationOptions(proposal);
      if (this.projectIdValue !== null) {
        this.append('workflow.paused', {
          by: 'policy',
          reason: `ambiguous intent (${proposal.kind}, confidence ${proposal.confidence.toFixed(2)})`,
          question: bounded(question, 2000),
          options: options.map((option) => bounded(option, 300)),
        });
        await this.settle();
      }
      return this.finish({
        kind: 'NEEDS_USER',
        question,
        options,
        reason: `ambiguous intent: ${proposal.rationale}`,
      });
    }

    let result: StepResult;
    try {
      result = await this.applyIntent(proposal, trimmed);
    } catch (error) {
      if (error instanceof NotInitializedError) {
        return this.finish({
          kind: 'NEEDS_USER',
          question: 'initialize the project first — describe your goal in plain language or run init',
          options: ['describe your goal'],
          reason: 'intent requires an initialized project',
        });
      }
      throw error;
    }
    this.append('intent.applied', {
      kind: proposal.kind,
      source: 'natural-language',
      confidence: proposal.confidence,
      mode: proposal.mode ?? null,
      autonomy: proposal.autonomy ?? null,
      artifactKind: proposal.artifactKind ?? null,
      overridden: false,
    });
    await this.settle();
    return result;
  }

  async runStep(options?: { readonly userAuthorized?: boolean }): Promise<StepResult> {
    this.assertInitialized();
    await this.settle();
    const state = this.state;
    if (state.paused !== null) {
      return this.finish(needsUserFromPause(state));
    }

    const mode = state.currentMode;
    const contract = MODE_CONTRACTS[mode];

    // A mode whose contractual artifact is already approved is complete; never
    // re-run it (that would needlessly revise the artifact). Evaluate its
    // transition directly. Reachable under 'off'/'until:<mode>' autonomy.
    // A pending user note (revision request, answered question) always means
    // re-work is intended, so it disables the shortcut. So does a persisted
    // user-sourced entry into this mode (`/mode` override or `/revise`): the
    // user explicitly asked to be here, so the next step must regenerate the
    // artifact as a new draft version rather than skip ahead. This keeps
    // revision requests working across process restarts, where the in-memory
    // note is gone but the `mode.entered` event (source 'user') remains.
    const hasPendingNote = this.pendingUserNote !== null && this.pendingUserNote.trim().length > 0;
    const lastTransition = state.transitions.at(-1);
    const enteredByUser = lastTransition !== undefined && lastTransition.to === mode && lastTransition.source === 'user';
    const completedId = hasPendingNote || enteredByUser ? undefined : state.artifactsByKind[contract.producedArtifact];
    const completed = completedId !== undefined ? state.artifacts[completedId] : undefined;
    if (
      completed !== undefined
      && completed.status === 'approved'
      && isContractualArtifactKind(completed.kind)
      && contract.nextMode !== null
    ) {
      this.emit({
        kind: 'notice',
        message: `${mode} is already complete (${completed.kind} v${completed.currentVersion} approved); evaluating the next transition`,
      });
      const outcome = evaluatePolicyTransition(this.state, {
        kind: 'mode-complete',
        evidence: [contract.completionEvidence],
      });
      return this.applyTransitionOutcome(outcome, {
        allowApprovalPause: false,
        ...(options?.userAuthorized === true ? { advanceAuthorized: 'user' as const } : {}),
      });
    }

    // Required-input guard: route back to the earliest affected stage.
    const reviseTarget = this.requiredInputRevision(state, mode);
    if (reviseTarget !== null) {
      const outcome: ControllerOutcome = {
        kind: 'REVISE',
        stage: reviseTarget.stage,
        reason: reviseTarget.reason,
      };
      this.append('transition.evaluated', transitionPayload(outcome, []));
      this.appendModeEntered(state.currentMode, reviseTarget.stage, reviseTarget.reason, 'auto');
      await this.settle();
      return this.finish(outcome);
    }

    // Build the mode's task packet (consumes any pending user note).
    const note = this.pendingUserNote;
    this.pendingUserNote = null;
    const context = await buildModeContext({
      state: this.state,
      goal: this.state.goal,
      workspaceRoot: this.services.workspaceRoot,
      artifacts: this.services.artifacts,
      tools: this.services.tools,
      ...(note !== null && note.trim().length > 0 ? { pendingUserNote: note } : {}),
    });

    const kind = contract.producedArtifact;
    const first = await this.runModeLoop(mode, [{ role: 'user', content: context }]);

    let draftParse: DraftParse | null = null;
    let failure: AgentLoopResult | null = null;
    if (first.status !== 'completed') {
      failure = first;
    } else {
      const parsed = parseArtifactDraft(kind, first.structuredOutput?.value);
      if (parsed.ok) {
        draftParse = parsed;
      } else {
        // One bounded corrective re-ask.
        const issues = parsed.issues.map((issue) => `- ${issue}`).join('\n');
        const correction: ModelMessage[] = [
          ...first.messages,
          {
            role: 'user',
            content:
              `Your previous response was invalid for the "${kind}" draft schema:\n${issues}\n` +
              'Respond ONLY with a corrected JSON object matching the schema. Fix every listed issue.',
          },
        ];
        const retry = await this.runModeLoop(mode, correction);
        if (retry.status !== 'completed') {
          failure = retry;
        } else {
          const reparsed = parseArtifactDraft(kind, retry.structuredOutput?.value);
          if (reparsed.ok) {
            draftParse = reparsed;
          } else {
            const route = this.describeRoute(mode);
            this.append('model.failed', {
              provider: route.provider,
              model: route.model,
              purpose: `mode:${mode}`,
              ...(route.role !== null ? { role: route.role } : {}),
              errorKind: 'invalid-response',
              message: bounded(
                `structured output failed validation twice: ${reparsed.issues.join('; ').slice(0, 800)}`,
                2000,
              ),
              attempts: 2,
              durationMs: 0,
            });
            return this.pauseFromFailure(
              `mode:${mode}`,
              `the ${kind} draft failed schema validation twice; the last issues were: ${reparsed.issues.slice(0, 3).join('; ')}`,
              'invalid structured output',
            );
          }
        }
      }
    }
    if (failure !== null) {
      return this.handleLoopFailure(`mode:${mode}`, failure);
    }
    if (draftParse === null) {
      throw new Error('internal error: mode step produced neither a draft nor a failure');
    }
    const draft = draftParse.draft;

    // Persist the artifact version.
    await this.settle();
    const beforeSave = this.state;
    const existingId = beforeSave.artifactsByKind[kind] ?? null;
    const existing = existingId !== null ? beforeSave.artifacts[existingId] : undefined;
    if (existing !== undefined) {
      const stored = await this.services.artifacts.save(kind, draft, {
        artifactId: existing.artifactId,
        previousVersion: existing.currentVersion,
      });
      this.append('artifact.revised', {
        artifactId: stored.artifactId,
        kind,
        fromVersion: existing.currentVersion,
        toVersion: stored.version,
        path: stored.path,
        reason: `re-run in ${mode}`,
      });
      await this.settle();
      this.emit({
        kind: 'artifact',
        artifactId: stored.artifactId,
        artifactKind: kind,
        version: stored.version,
        status: 'draft',
      });
    } else {
      const stored = await this.services.artifacts.save(kind, draft);
      const markdown = renderArtifactMarkdown(kind, draft);
      this.append('artifact.created', {
        artifactId: stored.artifactId,
        kind,
        title: bounded(stored.title, 300),
        version: 1,
        path: stored.path,
        summary: bounded(markdown.replace(/\s+/g, ' ').trim(), 300),
      });
      await this.settle();
      this.emit({
        kind: 'artifact',
        artifactId: stored.artifactId,
        artifactKind: kind,
        version: 1,
        status: 'draft',
      });
    }

    if (kind === 'verification-report') {
      const report = draft as {
        overallPass: boolean;
        results: readonly { criterion: string; status: 'pass' | 'fail' | 'unevaluated'; evidence: string }[];
      };
      const producedId = this.state.artifactsByKind[kind] ?? null;
      this.append('verification.completed', {
        passed: report.overallPass,
        results: report.results.map((result) => ({
          criterion: bounded(result.criterion, 2000),
          status: result.status,
          evidence: bounded(result.evidence, 4000),
        })),
        reportArtifactId: producedId,
      });
      await this.settle();
    }

    const outcome = evaluatePolicyTransition(this.state, {
      kind: 'mode-complete',
      evidence: [contract.completionEvidence],
    });
    return this.applyTransitionOutcome(outcome, {
      allowApprovalPause: true,
      ...(options?.userAuthorized === true ? { advanceAuthorized: 'user' as const } : {}),
    });
  }

  async runUntilPause(options?: { readonly maxSteps?: number }): Promise<StepResult> {
    this.assertInitialized();
    const maxSteps = options?.maxSteps ?? DEFAULT_MAX_STEPS;
    let last: StepResult | null = null;
    for (let step = 0; step < maxSteps; step += 1) {
      await this.settle();
      if (this.state.paused !== null) {
        return last ?? this.finish(needsUserFromPause(this.state));
      }
      const eventsBefore = this.state.stats.events;
      const result = await this.runStep();
      last = result;
      const outcome = result.outcome;
      if (outcome.kind === 'NEEDS_USER') return result;
      if (outcome.kind === 'REVISE') {
        this.emit({
          kind: 'notice',
          message: `routing back to ${outcome.stage}: ${outcome.reason}`,
        });
        return result;
      }
      if (outcome.kind === 'ADVANCE' && shouldAutoAdvance(this.state, outcome)) continue;
      if (
        outcome.kind === 'CONTINUE'
        && this.state.stats.events > eventsBefore
        && shouldAutoAdvance(this.state, outcome)
      ) {
        continue;
      }
      return result;
    }
    return this.finish({
      kind: 'NEEDS_USER',
      question: `step limit reached (${maxSteps} steps); run again to continue or raise the limit`,
      options: ['/status', '/why'],
      reason: 'runUntilPause hit its step limit',
    });
  }

  async approve(artifactId?: string): Promise<StepResult> {
    this.assertInitialized();
    await this.settle();
    const state = this.state;
    const currentKind = MODE_CONTRACTS[state.currentMode].producedArtifact;
    let targetId = artifactId ?? state.artifactsByKind[currentKind] ?? null;
    if (targetId === null) {
      const fallback = Object.values(state.artifacts).find(
        (candidate) => candidate.status === 'draft' && isContractualArtifactKind(candidate.kind),
      );
      targetId = fallback?.artifactId ?? null;
    }
    if (targetId === null) {
      this.emit({ kind: 'notice', message: 'nothing to approve: no draft contractual artifact exists' });
      return this.finish({
        kind: 'NEEDS_USER',
        question: 'nothing to approve yet — no draft contractual artifact exists',
        options: ['/status'],
        reason: 'approve found no target',
      });
    }
    const target = state.artifacts[targetId];
    if (target === undefined) {
      return this.finish({
        kind: 'NEEDS_USER',
        question: `unknown artifact "${targetId}"`,
        options: ['/status'],
        reason: 'approve target missing from state',
      });
    }
    if (target.status === 'approved') {
      this.emit({ kind: 'notice', message: `${target.kind} v${target.currentVersion} is already approved` });
      return this.finish({
        kind: 'CONTINUE',
        reason: `${target.kind} v${target.currentVersion} is already approved`,
      });
    }
    if (target.status === 'stale') {
      return this.finish({
        kind: 'NEEDS_USER',
        question: `${target.kind} is stale; regenerate it from its updated upstream inputs before approving`,
        options: ['/revise', '/status'],
        reason: 'refusing to approve a stale artifact',
      });
    }

    this.append('artifact.approved', {
      artifactId: target.artifactId,
      kind: target.kind,
      version: target.currentVersion,
      approvedBy: 'user',
    });
    await this.settle();
    // An approval pause ends with the user's approval: record the resume so
    // auto-advancement (which never runs while paused) can proceed.
    const paused = this.state.paused;
    if (paused !== null && paused.by === 'policy' && paused.options.includes('/approve')) {
      this.append('workflow.resumed', {
        by: 'user',
        note: `approved ${target.kind} v${target.currentVersion}`,
      });
      await this.settle();
    }
    this.emit({
      kind: 'artifact',
      artifactId: target.artifactId,
      artifactKind: target.kind,
      version: target.currentVersion,
      status: 'approved',
    });
    this.emit({ kind: 'notice', message: `approved ${target.kind} v${target.currentVersion}` });

    const outcome = evaluatePolicyTransition(this.state, {
      kind: 'artifact-approved',
      artifactId: target.artifactId,
    });
    // The approval IS the user's authorization to advance: apply it even
    // under autonomy 'off' (which only restricts automatic advancement).
    return this.applyTransitionOutcome(outcome, {
      allowApprovalPause: false,
      advanceAuthorized: 'user',
    });
  }

  async setMode(mode: WorkflowMode): Promise<StepResult> {
    this.assertInitialized();
    await this.settle();
    const state = this.state;
    try {
      assertTransitionAllowed(state, mode);
    } catch (error) {
      if (error instanceof PolicyRejectionError) {
        this.append('mode.transition.rejected', {
          from: state.currentMode,
          to: mode,
          reason: bounded(error.reason, 2000),
        });
        await this.settle();
        return this.finish({
          kind: 'NEEDS_USER',
          question: error.reason,
          options: ['/status', '/resume'],
          reason: `transition to ${mode} rejected by policy`,
        });
      }
      throw error;
    }
    this.appendModeEntered(state.currentMode, mode, 'user override', 'user');
    await this.settle();
    return this.finish({
      kind: 'CONTINUE',
      reason: `mode set to ${mode} by user override`,
    });
  }

  async setAutonomy(bound: AutonomyBound): Promise<StepResult> {
    this.assertInitialized();
    await this.settle();
    const state = this.state;
    this.append('autonomy.changed', { from: state.autonomy, to: bound, source: 'user' });
    await this.settle();
    this.emit({ kind: 'notice', message: `autonomy set to ${bound}` });
    return this.finish({ kind: 'CONTINUE', reason: `autonomy set to ${bound}` });
  }

  async pause(reason: string): Promise<StepResult> {
    this.assertInitialized();
    await this.settle();
    const state = this.state;
    if (state.paused !== null) {
      return this.finish(needsUserFromPause(state));
    }
    const trimmed = reason.trim().length > 0 ? reason.trim() : 'requested by user';
    this.append('workflow.paused', {
      by: 'user',
      reason: bounded(trimmed, 2000),
      question: null,
      options: [],
    });
    await this.settle();
    return this.finish({
      kind: 'NEEDS_USER',
      question: trimmed,
      options: ['/resume'],
      reason: 'paused by user',
    });
  }

  async resume(): Promise<StepResult> {
    this.assertInitialized();
    await this.settle();
    const state = this.state;
    if (state.paused === null) {
      this.emit({ kind: 'notice', message: 'the workflow is not paused' });
      return this.finish({ kind: 'CONTINUE', reason: 'the workflow is not paused' });
    }
    this.append('workflow.resumed', { by: 'user', note: 'resumed by user' });
    await this.settle();
    this.emit({ kind: 'notice', message: 'workflow resumed' });
    return this.finish({ kind: 'CONTINUE', reason: 'workflow resumed by user' });
  }

  async inspect(
    target: string,
  ): Promise<{ frontmatter: Record<string, unknown>; body: string; path: string } | null> {
    this.assertInitialized();
    const trimmed = target.trim();
    // Accept an artifact kind (e.g. `relay inspect prd`) by resolving it to
    // the latest stored artifact of that kind before delegating to the store.
    let resolved = trimmed;
    if (isArtifactKind(trimmed)) {
      const latest = await this.services.artifacts.latest(trimmed);
      if (latest !== null) resolved = latest.artifactId;
    }
    const found = await this.services.artifacts.inspect(resolved);
    if (found === null) return null;
    const artifactId = typeof found.frontmatter.id === 'string' ? found.frontmatter.id : resolved;
    this.append('artifact.inspected', { artifactId, via: 'user' });
    await this.settle();
    return { frontmatter: found.frontmatter, body: found.body, path: found.path };
  }

  statusView(): StatusView {
    this.assertInitialized();
    const loaded = this.services.repository.loadProject(
      this.projectIdValue ?? undefined,
    );
    return buildStatusView(loaded.state, loaded.events, this.sessionId);
  }

  whyView(): WhyView {
    this.assertInitialized();
    const loaded = this.services.repository.loadProject(
      this.projectIdValue ?? undefined,
    );
    return buildWhyView(loaded.state, loaded.events);
  }

  // -- intent application ------------------------------------------------------

  private async applyCommand(command: ParsedCommand): Promise<StepResult> {
    switch (command.command) {
      case 'status': {
        const view = this.statusView();
        this.emit({
          kind: 'notice',
          message:
            `status: ${view.mode} (${view.progress.stage}/${view.progress.total}) · ${view.artifacts.length} artifacts · ` +
            `${view.paused === null ? 'running' : `paused (${view.paused.by})`} · next: ${view.nextAction}`,
        });
        return this.finish({ kind: 'CONTINUE', reason: 'status requested' });
      }
      case 'why': {
        const view = this.whyView();
        const last = view.lastOutcome;
        this.emit({
          kind: 'notice',
          message:
            `why: mode ${view.mode}; last outcome ${last === null ? 'none recorded' : `${last.outcome} — ${last.reason}`}` +
            (view.pausedReason === null ? '' : `; paused: ${view.pausedReason}`),
        });
        return this.finish({ kind: 'CONTINUE', reason: 'explanation requested' });
      }
      case 'inspect': {
        const found = await this.inspect(command.target);
        if (found === null) {
          return this.finish({
            kind: 'NEEDS_USER',
            question: `no artifact matched "${command.target}"`,
            options: ['/status'],
            reason: 'inspect found no artifact',
          });
        }
        this.emit({
          kind: 'notice',
          message: `${found.path}:\n${found.body.slice(0, 400)}${found.body.length > 400 ? '…' : ''}`,
        });
        return this.finish({ kind: 'CONTINUE', reason: `inspected ${found.path}` });
      }
      case 'mode':
        return this.setMode(command.mode);
      case 'auto':
        return this.setAutonomy(command.bound);
      case 'pause':
        return this.pause(command.reason);
      case 'resume':
        return this.resume();
      case 'approve':
        return this.approve(command.artifactId);
      case 'revise':
        return this.reviseArtifactInternal(command.artifactKind, command.reason);
    }
  }

  private async applyIntent(proposal: IntentProposal, raw: string): Promise<StepResult> {
    switch (proposal.kind) {
      case 'provide-goal': {
        const goal = (proposal.freeText ?? '').trim() || raw;
        if (this.projectIdValue === null) {
          await this.initialize(goal);
        } else {
          this.pendingUserNote = goal;
        }
        return this.runStep();
      }
      case 'advance':
        return this.runStep({ userAuthorized: true });
      case 'change-mode': {
        if (proposal.mode === null || proposal.mode === undefined) {
          return this.finish({
            kind: 'NEEDS_USER',
            question: 'which mode should the workflow switch to?',
            options: ['/mode discovery', '/mode specification', '/status'],
            reason: 'change-mode intent without a mode',
          });
        }
        return this.setMode(proposal.mode);
      }
      case 'set-autonomy': {
        const bound =
          proposal.autonomy !== null && proposal.autonomy !== undefined
            ? parseAutonomyBound(proposal.autonomy)
            : null;
        if (bound === null) {
          return this.finish({
            kind: 'NEEDS_USER',
            question: 'which autonomy bound should be set (off, next-checkpoint, workflow, until:<mode>)?',
            options: ['/auto workflow', '/status'],
            reason: 'set-autonomy intent without a valid bound',
          });
        }
        return this.setAutonomy(bound);
      }
      case 'revise-artifact': {
        const kind = proposal.artifactKind ?? MODE_CONTRACTS[this.state.currentMode].producedArtifact;
        return this.reviseArtifactInternal(kind, proposal.freeText ?? raw);
      }
      case 'request-status': {
        const view = this.statusView();
        this.emit({
          kind: 'notice',
          message: `status: ${view.mode} (${view.progress.stage}/${view.progress.total}) · next: ${view.nextAction}`,
        });
        return this.finish({ kind: 'CONTINUE', reason: 'status requested' });
      }
      case 'request-explanation': {
        const view = this.whyView();
        const last = view.lastOutcome;
        this.emit({
          kind: 'notice',
          message:
            `why: mode ${view.mode}; last outcome ${last === null ? 'none recorded' : `${last.outcome} — ${last.reason}`}`,
        });
        return this.finish({ kind: 'CONTINUE', reason: 'explanation requested' });
      }
      case 'inspect-artifact': {
        const target = (proposal.freeText ?? '').trim();
        if (target.length === 0) {
          return this.finish({
            kind: 'NEEDS_USER',
            question: 'which artifact should be inspected?',
            options: ['/status', '/inspect <artifact>'],
            reason: 'inspect intent without a target',
          });
        }
        const found = await this.inspect(target);
        if (found === null) {
          return this.finish({
            kind: 'NEEDS_USER',
            question: `no artifact matched "${target}"`,
            options: ['/status'],
            reason: 'inspect found no artifact',
          });
        }
        this.emit({ kind: 'notice', message: `${found.path}:\n${found.body.slice(0, 400)}` });
        return this.finish({ kind: 'CONTINUE', reason: `inspected ${found.path}` });
      }
      case 'answer-question': {
        const answer = (proposal.freeText ?? '').trim() || raw;
        this.pendingUserNote = answer;
        if (this.state.paused !== null) {
          this.append('workflow.resumed', { by: 'user', note: bounded(answer, 2000) });
          await this.settle();
        }
        return this.runStep();
      }
      case 'pause':
        return this.pause(proposal.freeText ?? 'paused via user request');
      case 'unknown':
        return this.finish({
          kind: 'NEEDS_USER',
          question: 'could not determine what to do with that request',
          options: ['/status', '/why'],
          reason: 'unknown intent',
        });
    }
  }

  private async reviseArtifactInternal(kind: ArtifactKind, reason: string): Promise<StepResult> {
    this.assertInitialized();
    await this.settle();
    const state = this.state;
    if (state.artifactsByKind[kind] === undefined) {
      this.emit({ kind: 'notice', message: `no ${kind} to revise yet` });
      return this.finish({
        kind: 'NEEDS_USER',
        question: `no ${kind} to revise yet`,
        options: ['/status'],
        reason: 'revision requested for a missing artifact',
      });
    }
    const trimmed = reason.trim().length > 0 ? reason.trim() : 'user requested revision';
    const target = kindToMode(kind);
    this.appendModeEntered(
      state.currentMode,
      target,
      `revision requested: ${trimmed}`,
      'user',
    );
    this.pendingUserNote = trimmed;
    await this.settle();
    this.emit({
      kind: 'notice',
      message: `revising ${kind} — downstream artifacts will be marked stale`,
    });
    return this.finish({
      kind: 'CONTINUE',
      reason: `revision of ${kind} requested; routed to ${target}`,
    });
  }

  // -- model step machinery ----------------------------------------------------

  private async runModeLoop(
    mode: WorkflowMode,
    messages: readonly ModelMessage[],
  ): Promise<AgentLoopResult> {
    const state = this.state;
    const routes = this.resolveRoutes(`mode:${mode}`, mode);
    const schema = draftJsonSchema(MODE_CONTRACTS[mode].producedArtifact);
    return this.withControllerCancellation((token) =>
      runAgentLoop({
        routes,
        system: modePrompt(mode),
        purpose: `mode:${mode}`,
        mode,
        initialMessages: messages,
        tools: this.services.tools,
        permittedCapabilities: MODE_CONTRACTS[mode].permittedToolCapabilities,
        permissionGate: createPermissionGate({ mode, autonomy: state.autonomy, state }),
        budgets: this.newBudgetTracker(),
        cancellation: token,
        structuredOutput: { name: 'artifact-draft', schema },
        workspaceRoot: this.services.workspaceRoot,
        onEvent: (event) => this.handleLoopEvent(event, mode),
      }),
    );
  }

  private async classifyIntent(
    input: string,
  ): Promise<{ proposal: IntentProposal } | { failure: StepResult } | null> {
    const mode = this.projectIdValue !== null ? this.state.currentMode : null;
    const routes = this.resolveRoutes('intent', mode);
    const result = await this.withControllerCancellation((token) =>
      runAgentLoop({
        routes,
        system: INTENT_SYSTEM_PROMPT,
        purpose: 'intent',
        mode,
        initialMessages: [
          {
            role: 'user',
            content: `User input:\n${bounded(input, 4000)}\n\nCurrent project state:\n${this.compactStateSummary()}`,
          },
        ],
        tools: this.services.tools,
        permittedCapabilities: [],
        permissionGate: () => ({
          decision: 'deny',
          reason: 'intent classification runs without tools',
          needsUser: false,
        }),
        budgets: this.newBudgetTracker(),
        cancellation: token,
        structuredOutput: {
          name: 'intent-proposal',
          schema: intentProposalSchema.toJSONSchema({ target: 'draft-2020-12' }) as JsonSchemaObject,
        },
        workspaceRoot: this.services.workspaceRoot,
        onEvent: (event) => this.handleLoopEvent(event, mode),
      }),
    );
    if (result.status !== 'completed') {
      return { failure: await this.handleLoopFailure('intent', result) };
    }
    const parsed = intentProposalSchema.safeParse(result.structuredOutput?.value);
    if (!parsed.success) {
      return null;
    }
    return { proposal: parsed.data };
  }

  /** Link the controller's abort signal to a fresh cancellation token. */
  private async withControllerCancellation<T>(
    run: (token: CancellationToken) => Promise<T>,
  ): Promise<T> {
    const { token, cancel } = CancellationToken.create();
    const onAbort = (): void => cancel('canceled by controller signal');
    let linked = false;
    if (this.signal !== undefined) {
      if (this.signal.aborted) {
        cancel('canceled before the step started');
      } else {
        this.signal.addEventListener('abort', onAbort, { once: true });
        linked = true;
      }
    }
    try {
      return await run(token);
    } finally {
      if (linked) this.signal?.removeEventListener('abort', onAbort);
    }
  }

  /** Translate agent-loop events into relay event appends and UI events. */
  private handleLoopEvent(event: AgentLoopEvent, mode: WorkflowMode | null): void {
    switch (event.kind) {
      case 'model-request':
        this.append('model.requested', {
          provider: event.provider,
          model: event.model,
          purpose: bounded(event.purpose, 200),
          mode,
          attempt: event.attempt,
          ...(event.role !== null ? { role: event.role } : {}),
        });
        break;
      case 'model-response':
        this.append('model.responded', {
          provider: event.provider,
          model: event.model,
          purpose: bounded(event.purpose, 200),
          ...(event.role !== null ? { role: event.role } : {}),
          usage: {
            inputTokens: Math.max(0, Math.floor(event.usage.inputTokens)),
            outputTokens: Math.max(0, Math.floor(event.usage.outputTokens)),
          },
          toolCalls: Math.max(0, Math.floor(event.toolCalls)),
          durationMs: Math.max(0, Math.floor(event.durationMs)),
        });
        break;
      case 'model-failure':
        this.append('model.failed', {
          provider: event.provider,
          model: event.model,
          purpose: bounded(event.purpose, 200),
          ...(event.role !== null ? { role: event.role } : {}),
          errorKind: event.errorKind,
          message: bounded(event.message, 2000),
          attempts: event.attempts,
          durationMs: Math.max(0, Math.floor(event.durationMs)),
        });
        break;
      case 'text-delta':
        this.emit({ kind: 'delta', text: event.text });
        break;
      case 'tool-start':
        this.emit({ kind: 'tool', tool: event.tool, capability: event.capability, phase: 'start' });
        break;
      case 'tool-end':
        this.append('tool.executed', {
          tool: event.tool,
          capability: event.capability,
          mode,
          outcome: event.ok ? 'ok' : 'error',
          durationMs: Math.max(0, Math.floor(event.durationMs)),
          summary: bounded(event.summary, 2000),
          affectedFiles: event.affectedFiles.slice(0, 500).map((file) => bounded(file, 1000)),
        });
        this.emit({
          kind: 'tool',
          tool: event.tool,
          capability: event.capability,
          phase: 'end',
          ok: event.ok,
          summary: bounded(event.summary, 200),
        });
        break;
      case 'tool-denied':
        this.append('tool.denied', {
          tool: event.tool,
          capability: event.capability,
          mode,
          reason: bounded(event.reason, 2000),
        });
        break;
    }
  }

  private async handleLoopFailure(phase: string, result: AgentLoopResult): Promise<StepResult> {
    await this.settle();
    const message = result.error?.message ?? `the model step ended with status "${result.status}"`;
    if (result.status === 'canceled') {
      this.append('run.interrupted', { phase: bounded(phase, 200), hint: bounded(message, 2000) });
      await this.settle();
      return this.finish({
        kind: 'NEEDS_USER',
        question: `the run was canceled: ${message}`,
        options: ['/resume', '/status'],
        reason: 'canceled',
      });
    }
    return this.pauseFromFailure(phase, message, `${result.status}: ${message}`);
  }

  /** Pause (unless already paused) and surface an actionable NEEDS_USER. */
  private async pauseFromFailure(
    phase: string,
    question: string,
    reason: string,
  ): Promise<StepResult> {
    if (this.state.paused === null) {
      this.append('workflow.paused', {
        by: 'policy',
        reason: bounded(`${phase} failed: ${reason}`, 2000),
        question: bounded(question, 2000),
        options: ['retry', 'pause', '/why'],
      });
    }
    await this.settle();
    return this.finish({
      kind: 'NEEDS_USER',
      question,
      options: ['retry', 'pause', '/why'],
      reason: bounded(`${phase} failed: ${reason}`, 2000),
    });
  }

  /** Shared post-artifact transition application (runStep step 9, approve). */
  private async applyTransitionOutcome(
    outcome: ControllerOutcome,
    options: { readonly allowApprovalPause: boolean; readonly advanceAuthorized?: 'user' },
  ): Promise<StepResult> {
    const state = this.state;

    if (outcome.kind === 'NEEDS_USER') {
      const currentKind = MODE_CONTRACTS[state.currentMode].producedArtifact;
      const producedId = state.artifactsByKind[currentKind];
      const produced = producedId !== undefined ? state.artifacts[producedId] : undefined;
      const isApproval =
        options.allowApprovalPause
        && produced !== undefined
        && produced.status === 'draft'
        && isContractualArtifactKind(currentKind);
      if (isApproval && produced !== undefined) {
        this.append('artifact.approval.requested', {
          artifactId: produced.artifactId,
          kind: currentKind,
          version: produced.currentVersion,
          reason: bounded(`approval required to complete ${state.currentMode}`, 2000),
        });
        this.append('workflow.paused', {
          by: 'policy',
          reason: `Approval required for ${currentKind} v${produced.currentVersion}`,
          question: bounded(outcome.question, 2000),
          options: ['/approve', `/revise ${currentKind} <reason>`],
        });
        this.append('transition.evaluated', transitionPayload(outcome, []));
        await this.settle();
        return this.finish(outcome);
      }
      if (state.currentMode === 'delivery') {
        const intervention = requiresUserIntervention(state, 'delivery');
        this.append('transition.evaluated', transitionPayload(outcome, []));
        this.append('workflow.paused', {
          by: 'policy',
          reason: bounded(intervention.reason, 2000),
          question: bounded(outcome.question, 2000),
          options: outcome.options.map((option) => bounded(option, 300)),
        });
        await this.settle();
        return this.finish(outcome);
      }
      this.append('transition.evaluated', transitionPayload(outcome, []));
      await this.settle();
      return this.finish(outcome);
    }

    if (outcome.kind === 'ADVANCE') {
      this.append('transition.evaluated', transitionPayload(outcome, []));
      // `advanceAuthorized: 'user'` marks advancement the user explicitly
      // authorized (an approval, or an explicit "advance" intent). Autonomy
      // bounds only limit AUTOMATIC advancement, so these apply regardless
      // of the bound — otherwise `off` deadlocks after every checkpoint.
      const authorized = options.advanceAuthorized === 'user';
      if (authorized || shouldAutoAdvance(this.state, outcome)) {
        this.appendModeEntered(
          this.state.currentMode,
          outcome.to,
          outcome.reason,
          authorized ? 'user' : 'auto',
        );
        await this.settle();
      }
      return this.finish(outcome);
    }

    if (outcome.kind === 'REVISE') {
      this.append('transition.evaluated', transitionPayload(outcome, []));
      this.appendModeEntered(this.state.currentMode, outcome.stage, outcome.reason, 'auto');
      await this.settle();
      return this.finish(outcome);
    }

    this.append('transition.evaluated', transitionPayload(outcome, []));
    await this.settle();
    return this.finish(outcome);
  }

  /** Missing or stale required inputs route work back to the earliest stage. */
  private requiredInputRevision(
    state: ProjectState,
    mode: WorkflowMode,
  ): { stage: WorkflowMode; reason: string } | null {
    for (const kind of MODE_CONTRACTS[mode].requiredInputs) {
      const artifactId = state.artifactsByKind[kind];
      if (artifactId === undefined) {
        return {
          stage: KIND_TO_MODE[kind],
          reason:
            `the required "${kind}" input does not exist yet; routing to ${KIND_TO_MODE[kind]} to produce it`,
        };
      }
      const artifact = state.artifacts[artifactId];
      if (artifact === undefined) {
        return {
          stage: KIND_TO_MODE[kind],
          reason: `the required "${kind}" input is missing from project state; routing to ${KIND_TO_MODE[kind]}`,
        };
      }
      if (artifact.status === 'stale') {
        const stage = earliestAffectedStage(state, kind);
        return {
          stage,
          reason:
            `the required "${kind}" artifact (${artifactId}) is stale; routing to ${stage} ` +
            '(the earliest stage affected) to regenerate it',
        };
      }
    }
    return null;
  }

  /**
   * Resolve the route chain for a call, applying invocation overrides in the
   * documented precedence (later wins): the configured route, `--role`, then
   * `--provider` (filter), then `--model` (rewrite).
   */
  private resolveRoutes(purpose: string, mode: WorkflowMode | null): readonly ResolvedRoute[] {
    let routes = this.services.router.routeChain({ mode, purpose });
    if (this.roleOverride !== undefined) {
      // Throws an actionable RouteUnavailableError for unknown roles.
      routes = this.services.router.chainForRole(this.roleOverride);
    }
    const overrides = this.providerOverrides;
    if (overrides === undefined) return routes;
    if (overrides.provider !== undefined) {
      const provider = overrides.provider;
      routes = routes.filter((route) => route.providerName === provider);
      if (routes.length === 0) {
        throw new RouteUnavailableError(
          `provider override "${provider}" matched no route in the chain for purpose "${purpose}"`,
        );
      }
    }
    const model = overrides.model;
    if (model !== undefined) {
      routes = routes.map((route) => ({ ...route, model }));
    }
    return routes;
  }

  private describeRoute(mode: WorkflowMode): { provider: string; model: string; role: string | null } {
    const routes = this.resolveRoutes(`mode:${mode}`, mode);
    const first = routes[0];
    return first !== undefined
      ? { provider: first.providerName, model: first.model, role: first.role }
      : { provider: 'unknown', model: 'unknown', role: null };
  }

  private newBudgetTracker(): BudgetTracker {
    return new BudgetTracker(this.services.budgets, {
      onExhausted: (budget, detail) => {
        const limits = this.services.budgets as unknown as Record<string, number | undefined>;
        this.append('budget.exhausted', {
          budget: bounded(budget, 100),
          limit: limits[budget] ?? 0,
          detail: bounded(detail, 2000),
        });
      },
    });
  }

  private compactStateSummary(): string {
    if (this.projectIdValue === null) {
      return 'Project not initialized yet (no goal recorded).';
    }
    const state = this.state;
    const artifacts = Object.values(state.artifacts)
      .map((artifact) => `${artifact.kind}@v${artifact.currentVersion}(${artifact.status})`)
      .join(', ');
    return [
      `goal: ${bounded(state.goal, 200)}`,
      `current mode: ${state.currentMode}`,
      `autonomy: ${state.autonomy}`,
      `artifacts: ${artifacts.length > 0 ? artifacts : 'none yet'}`,
      `stale artifacts: ${state.staleArtifacts.length > 0 ? state.staleArtifacts.join(', ') : 'none'}`,
      `paused: ${state.paused === null ? 'no' : `yes (${state.paused.reason})`}`,
      `verification: ${state.verification.passed === null ? 'not run' : state.verification.passed ? 'passed' : 'failed'}`,
    ].join('\n');
  }

  // -- event append plumbing ---------------------------------------------------

  private append<T extends RelayEventType>(type: T, payload: EventPayloadOf<T>): void {
    if (this.projectIdValue === null) {
      // Classification telemetry for a not-yet-initialized project cannot be
      // appended; the UI stream still carries it.
      return;
    }
    const input = {
      type,
      projectId: this.projectIdValue,
      sessionId: this.sessionId,
      payload,
    } as EventAppendInput;
    const pending = this.services.eventStore.append(input).then(() => {
      if (this.projectIdValue !== null) {
        const loaded = this.services.repository.loadProject(this.projectIdValue);
        this.stateCache = loaded.state;
      }
    });
    this.pendingAppends.push(pending);
  }

  private appendModeEntered(
    from: WorkflowMode | null,
    to: WorkflowMode,
    reason: string,
    source: 'auto' | 'user',
  ): void {
    this.append('mode.entered', {
      from,
      to,
      reason: bounded(reason, 2000),
      source,
    });
    this.emit({ kind: 'mode', mode: to, previous: from, reason });
  }

  private async drainAppends(): Promise<void> {
    const pending = this.pendingAppends.splice(0, this.pendingAppends.length);
    await Promise.all(pending);
  }

  /** Wait for queued appends and recompute state from the log. */
  private async settle(): Promise<void> {
    await this.drainAppends();
    if (this.projectIdValue !== null) {
      const loaded = this.services.repository.loadProject(this.projectIdValue);
      this.stateCache = loaded.state;
    }
  }

  private emit(event: ControllerUiEvent): void {
    for (const listener of this.listeners) {
      try {
        listener(event);
      } catch {
        // A misbehaving UI listener must never break the workflow.
      }
    }
  }

  private finish(outcome: ControllerOutcome): StepResult {
    this.emit({ kind: 'outcome', outcome });
    return {
      outcome,
      state: this.stateCache ?? createEmptyProjectState(),
    };
  }

  private assertInitialized(): void {
    if (this.projectIdValue === null || this.stateCache === null) {
      throw new NotInitializedError('this operation requires an initialized project');
    }
  }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function bounded(text: string, max: number): string {
  if (text.length <= max) return text;
  return `${text.slice(0, Math.max(0, max - 1))}…`;
}

function transitionPayload(
  outcome: ControllerOutcome,
  evidence: readonly string[],
): {
  outcome: 'CONTINUE' | 'ADVANCE' | 'NEEDS_USER' | 'REVISE';
  to: WorkflowMode | null;
  reviseStage: WorkflowMode | null;
  reason: string;
  evidence: string[];
} {
  return {
    outcome: outcome.kind,
    to: outcome.kind === 'ADVANCE' ? outcome.to : null,
    reviseStage: outcome.kind === 'REVISE' ? outcome.stage : null,
    reason: bounded(outcome.reason, 2000),
    evidence: evidence.map((entry) => bounded(entry, 1000)).slice(0, 20),
  };
}

function needsUserFromPause(state: ProjectState): ControllerOutcome {
  const paused = state.paused;
  if (paused === null) {
    return { kind: 'CONTINUE', reason: 'not paused' };
  }
  return {
    kind: 'NEEDS_USER',
    question: paused.question ?? `workflow is paused: ${paused.reason}`,
    options: paused.options.length > 0 ? [...paused.options] : ['/resume'],
    reason: paused.reason,
  };
}

function draftJsonSchema(kind: ArtifactKind): JsonSchemaObject {
  return ARTIFACT_DRAFT_SCHEMAS[kind].toJSONSchema({ target: 'draft-2020-12' }) as JsonSchemaObject;
}

function proposalForCommand(command: ParsedCommand): IntentProposal {
  const base = { confidence: 1, rationale: 'explicit command' };
  switch (command.command) {
    case 'status':
      return intentProposalSchema.parse({ kind: 'request-status', ...base });
    case 'why':
      return intentProposalSchema.parse({ kind: 'request-explanation', ...base });
    case 'inspect':
      return intentProposalSchema.parse({ kind: 'inspect-artifact', ...base, freeText: command.target });
    case 'mode':
      return intentProposalSchema.parse({ kind: 'change-mode', ...base, mode: command.mode });
    case 'auto':
      return intentProposalSchema.parse({ kind: 'set-autonomy', ...base, autonomy: command.bound });
    case 'pause':
      return intentProposalSchema.parse({ kind: 'pause', ...base, freeText: command.reason });
    case 'resume':
      return intentProposalSchema.parse({ kind: 'advance', ...base, rationale: 'explicit /resume command' });
    case 'approve':
      return intentProposalSchema.parse({ kind: 'advance', ...base, rationale: 'explicit /approve command' });
    case 'revise':
      return intentProposalSchema.parse({
        kind: 'revise-artifact',
        ...base,
        artifactKind: command.artifactKind,
        freeText: command.reason,
      });
  }
}

function appliedForCommand(command: ParsedCommand): {
  kind: string;
  source: 'command';
  confidence: number;
  mode: WorkflowMode | null;
  autonomy: string | null;
  artifactKind: ArtifactKind | null;
  overridden: boolean;
} {
  const proposal = proposalForCommand(command);
  return {
    kind: proposal.kind,
    source: 'command',
    confidence: proposal.confidence,
    mode: proposal.mode ?? null,
    autonomy: proposal.autonomy ?? null,
    artifactKind: proposal.artifactKind ?? null,
    overridden: false,
  };
}

const INTENT_DESCRIPTIONS: Readonly<Record<string, string>> = {
  'provide-goal': 'set or refine the project goal',
  advance: 'continue the workflow',
  'revise-artifact': 'revise an existing artifact',
  'change-mode': 'switch the workflow mode',
  'set-autonomy': 'change how far Relay runs without asking',
  'answer-question': 'answer the pending question',
  'request-status': 'see the current status',
  'request-explanation': 'see an explanation of the last decision',
  'inspect-artifact': 'inspect a stored artifact',
  pause: 'pause the workflow',
  unknown: 'do something else',
};

function focusedQuestion(proposal: IntentProposal, raw: string): string {
  const description = INTENT_DESCRIPTIONS[proposal.kind] ?? proposal.kind;
  const target =
    proposal.mode !== null && proposal.mode !== undefined
      ? `switch to ${proposal.mode}`
      : proposal.artifactKind !== null && proposal.artifactKind !== undefined
        ? `revise the ${proposal.artifactKind}`
        : description;
  return (
    `I'm not sure what you meant by "${bounded(raw, 140)}". ` +
    `My best guess is you want to ${target} (confidence ${proposal.confidence.toFixed(2)}). ` +
    'Confirm with a clearer instruction or rephrase the request.'
  );
}

function interpretationOptions(proposal: IntentProposal): readonly string[] {
  if (proposal.kind === 'change-mode' && proposal.mode !== null && proposal.mode !== undefined) {
    return [`yes — switch to ${proposal.mode}`, 'no — rephrase the request'];
  }
  if (
    proposal.kind === 'set-autonomy'
    && proposal.autonomy !== null
    && proposal.autonomy !== undefined
  ) {
    return [`yes — set autonomy to ${proposal.autonomy}`, 'no — rephrase the request'];
  }
  if (
    proposal.kind === 'revise-artifact'
    && proposal.artifactKind !== null
    && proposal.artifactKind !== undefined
  ) {
    return [`yes — revise the ${proposal.artifactKind}`, 'no — rephrase the request'];
  }
  return [`proceed: ${INTENT_DESCRIPTIONS[proposal.kind] ?? proposal.kind}`, 'rephrase the request'];
}
