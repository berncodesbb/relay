/**
 * Test stub for the workflow controller's TUI-facing surface.
 *
 * Implements the structural `TuiController` port with canned views, recorded
 * calls, and scriptable event emission so App rendering can be tested without
 * storage, providers, or a real workflow.
 */

import { modeIndex } from '../../../src/core/modes.js';
import { createEmptyProjectState, type ProjectState } from '../../../src/core/project-state.js';
import type { StatusView, WhyView } from '../../../src/runtime/controller-views.js';
import type { ControllerUiEvent, TuiController } from '../../../src/tui/view-model.js';

export type StubController = TuiController & {
  emit(event: ControllerUiEvent): void;
  setState(state: ProjectState): void;
  setProjectId(projectId: string | null): void;
  readonly handleInputCalls: readonly string[];
  readonly initializeCalls: readonly string[];
  readonly inspectCalls: readonly string[];
  readonly runStepCalls: number;
};

export type StubOptions = {
  readonly state?: Partial<ProjectState>;
  readonly projectId?: string | null;
  readonly statusView?: StatusView;
  readonly whyView?: WhyView;
  readonly inspectResult?: unknown;
  readonly inspectError?: Error;
  readonly handleInputImpl?: (input: string) => Promise<void>;
};

function defaultStatusView(state: ProjectState): StatusView {
  return {
    goal: state.goal,
    mode: state.currentMode,
    autonomy: state.autonomy,
    paused: state.paused === null
      ? null
      : {
        by: state.paused.by,
        reason: state.paused.reason,
        question: state.paused.question,
        options: state.paused.options,
      },
    progress: { stage: modeIndex(state.currentMode) + 1, total: 7 },
    artifacts: [],
    staleArtifacts: state.staleArtifacts,
    changedFiles: [],
    blockers: [],
    nextAction: 'draft the opportunity brief',
    stats: {
      events: state.stats.events,
      modelCalls: state.stats.modelCalls,
      toolCalls: state.stats.toolCalls,
    },
  };
}

function defaultWhyView(state: ProjectState): WhyView {
  return {
    mode: state.currentMode,
    lastOutcome: state.lastOutcome === null
      ? null
      : {
        outcome: state.lastOutcome.outcome,
        reason: state.lastOutcome.reason,
        evidence: state.lastOutcome.evidence,
        at: state.lastOutcome.at,
      },
    recentTransitions: state.transitions.slice(-5).map((transition) => ({
      from: transition.from,
      to: transition.to,
      reason: transition.reason,
      at: transition.at,
      source: transition.source,
    })),
    rejectedTransitions: [],
    routing: [],
    staleArtifacts: state.staleArtifacts,
    pausedReason: state.paused === null ? null : state.paused.reason,
  };
}

export function createStubController(options: StubOptions = {}): StubController {
  let state: ProjectState = { ...createEmptyProjectState(), ...options.state };
  let projectId: string | null = options.projectId ?? null;
  const listeners = new Set<(event: ControllerUiEvent) => void>();
  const handleInputCalls: string[] = [];
  const initializeCalls: string[] = [];
  const inspectCalls: string[] = [];
  let runStepCalls = 0;

  const assertInitialized = (): void => {
    if (projectId === null) {
      throw new Error('No Relay project is initialized: stub requires an initialized project');
    }
  };

  const stub: StubController = {
    get projectId() {
      return projectId;
    },
    get state() {
      // Mirrors the real controller: reading state before initialization throws.
      assertInitialized();
      return state;
    },
    get handleInputCalls() {
      return handleInputCalls;
    },
    get initializeCalls() {
      return initializeCalls;
    },
    get inspectCalls() {
      return inspectCalls;
    },
    get runStepCalls() {
      return runStepCalls;
    },
    subscribe(listener: (event: ControllerUiEvent) => void): () => void {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    async handleInput(input: string): Promise<void> {
      handleInputCalls.push(input);
      await options.handleInputImpl?.(input);
    },
    async runStep(): Promise<void> {
      runStepCalls += 1;
    },
    async initialize(goal: string): Promise<void> {
      initializeCalls.push(goal);
      projectId = 'proj_stub';
      state = { ...state, goal };
    },
    statusView(): StatusView {
      assertInitialized();
      return options.statusView ?? defaultStatusView(state);
    },
    whyView(): WhyView {
      assertInitialized();
      return options.whyView ?? defaultWhyView(state);
    },
    async inspect(target: string): Promise<unknown> {
      assertInitialized();
      inspectCalls.push(target);
      if (options.inspectError !== undefined) {
        throw options.inspectError;
      }
      return options.inspectResult ?? { markdown: '# Opportunity Brief (stub)\n\nproblem: too many tabs' };
    },
    emit(event: ControllerUiEvent): void {
      for (const listener of listeners) {
        listener(event);
      }
    },
    setState(next: ProjectState): void {
      state = next;
    },
    setProjectId(next: string | null): void {
      projectId = next;
    },
  };
  return stub;
}
