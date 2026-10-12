/**
 * End-to-end workflow integration: WorkflowController + event store +
 * repository + artifact store + tools + agent loop, driven by the
 * deterministic sample-repo fake provider against a copied fixture
 * repository. Covers the full seven-mode run with approval gates, the
 * auto-approve happy path, revision loops with staleness routing,
 * restart/resume, ambiguous input, provider-failure and budget recovery,
 * verification independence, the demo provider, and the command surface.
 */

import { spawnSync } from 'node:child_process';
import { cpSync, mkdtempSync, readdirSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { afterEach, describe, expect, it } from 'vitest';
import { ARTIFACT_KINDS, type ArtifactKind } from '../../src/core/artifacts.js';
import type { AutonomyBound } from '../../src/core/modes.js';
import { replayProject } from '../../src/core/reducer.js';
import { DEFAULT_CONFIG } from '../../src/config/defaults.js';
import type { BudgetLimits, RelayConfig } from '../../src/config/schema.js';
import { FakeProvider } from '../../src/providers/fake.js';
import type { ModelProvider, ModelRequest } from '../../src/providers/provider.js';
import {
  NotInitializedError,
  ProjectAlreadyInitializedError,
  WorkflowController,
  type ControllerUiEvent,
  type StepResult,
} from '../../src/runtime/controller.js';
import { createDemoFakeProvider, DEMO_GOAL } from '../../src/runtime/demo-provider.js';
import { ModelRouter } from '../../src/runtime/model-router.js';
import { ArtifactMarkdownStore } from '../../src/artifacts/markdown-store.js';
import { openDatabase, type RelayDatabase } from '../../src/storage/database.js';
import { SqliteEventStore } from '../../src/storage/event-store.js';
import { runMigrations } from '../../src/storage/migrations.js';
import { relayPaths } from '../../src/storage/paths.js';
import { ProjectRepository } from '../../src/storage/project-repository.js';
import { createDefaultToolRegistry } from '../../src/tools/index.js';
import {
  createWorkflowFakeProvider,
  sampleWorkflowSteps,
  SAMPLE_GOAL,
} from '../fixtures/sample-repo-script.js';

const FIXTURE_DIR = join(dirname(fileURLToPath(import.meta.url)), '..', 'fixtures', 'sample-repo');
const DELIVERY_HOLD = /delivery actions require explicit authorization/i;

type Harness = {
  readonly workspace: string;
  readonly db: RelayDatabase;
  readonly controller: WorkflowController;
  readonly artifacts: ArtifactMarkdownStore;
  readonly repository: ProjectRepository;
};

const openDbs: RelayDatabase[] = [];
const workspaces: string[] = [];

function fakeRouting(): RelayConfig['routing'] {
  const entry = { provider: 'fake' as const, model: 'fake-standard' };
  return {
    discovery: entry,
    specification: entry,
    'technical-design': entry,
    planning: entry,
    implementation: entry,
    verification: entry,
    delivery: entry,
  };
}

function makeSampleWorkspace(): string {
  const workspace = mkdtempSync(join(tmpdir(), 'relay-workflow-'));
  for (const entry of readdirSync(FIXTURE_DIR)) {
    cpSync(join(FIXTURE_DIR, entry), join(workspace, entry), { recursive: true });
  }
  const init = spawnSync('git', ['init'], { cwd: workspace, encoding: 'utf8' });
  if (init.status !== 0) {
    throw new Error(`git init failed in test workspace: ${init.stderr}`);
  }
  const add = spawnSync('git', ['add', '-A'], { cwd: workspace, encoding: 'utf8' });
  if (add.status !== 0) {
    throw new Error(`git add failed in test workspace: ${add.stderr}`);
  }
  workspaces.push(workspace);
  return workspace;
}

function makeBareWorkspace(): string {
  const workspace = mkdtempSync(join(tmpdir(), 'relay-demo-'));
  workspaces.push(workspace);
  return workspace;
}

function createHarness(options: {
  readonly provider: ModelProvider;
  readonly workspace: string;
  readonly budgets?: Partial<BudgetLimits>;
  readonly initialAutonomy?: AutonomyBound;
}): Harness {
  const paths = relayPaths(options.workspace);
  const db = openDatabase(paths.database);
  runMigrations(db);
  openDbs.push(db);
  const store = new SqliteEventStore(db);
  const repository = new ProjectRepository(db, store, replayProject);
  const artifacts = new ArtifactMarkdownStore(paths.artifactsDir);
  const tools = createDefaultToolRegistry(DEFAULT_CONFIG.tools);
  const router = new ModelRouter(new Map([['fake', options.provider]]), fakeRouting());
  const controller = new WorkflowController(
    {
      workspaceRoot: options.workspace,
      repository,
      eventStore: store,
      artifacts,
      router,
      tools,
      budgets: { ...DEFAULT_CONFIG.budgets, ...options.budgets },
    },
    { initialAutonomy: options.initialAutonomy ?? 'workflow' },
  );
  return { workspace: options.workspace, db, controller, artifacts, repository };
}

/** The NEEDS_USER question (or the outcome reason for non-question outcomes). */
function questionOf(result: StepResult): string {
  const outcome = result.outcome;
  return outcome.kind === 'NEEDS_USER' ? outcome.question : outcome.reason;
}

/** Run steps, approving each contractual pause, until the delivery hold. */
async function driveToDeliveryHold(controller: WorkflowController): Promise<StepResult> {
  for (let index = 0; index < 40; index += 1) {
    const result = await controller.runStep();
    if (result.outcome.kind === 'NEEDS_USER') {
      if (DELIVERY_HOLD.test(result.outcome.question)) return result;
      expect(result.state.paused?.by).toBe('policy');
      const approval = await controller.approve();
      expect(approval.outcome.kind).toBe('ADVANCE');
      continue;
    }
    expect(['ADVANCE', 'CONTINUE']).toContain(result.outcome.kind);
  }
  throw new Error('did not reach the delivery hold within the step budget');
}

afterEach(() => {
  while (openDbs.length > 0) {
    const db = openDbs.pop();
    try {
      db?.close();
    } catch {
      // Already closed by the test.
    }
  }
  for (const workspace of workspaces) {
    rmSync(workspace, { recursive: true, force: true });
  }
  workspaces.length = 0;
});

describe('end-to-end workflow (sample repo fixture)', () => {
  it('drives all seven modes with explicit approvals, seven artifacts, and no commits', async () => {
    const workspace = makeSampleWorkspace();
    const harness = createHarness({ provider: createWorkflowFakeProvider(), workspace });
    const uiEvents: ControllerUiEvent[] = [];
    harness.controller.subscribe((event) => uiEvents.push(event));

    await harness.controller.initialize(SAMPLE_GOAL);
    const final = await driveToDeliveryHold(harness.controller);

    // Every contractual checkpoint paused for the user and required approve().
    const approvals = uiEvents.filter(
      (event) => event.kind === 'artifact' && event.status === 'approved',
    );
    expect(approvals).toHaveLength(5); // brief, prd, trd, plan, verification-report

    // All seven artifact kinds exist as Markdown under .relay/artifacts.
    for (const kind of ARTIFACT_KINDS) {
      const stored = await harness.artifacts.list(kind);
      expect(stored.length, kind).toBeGreaterThanOrEqual(1);
      expect(stored[0]?.path).toMatch(/^\.relay[/\\]artifacts[/\\]/);
    }

    // Delivery ends in the policy hold, paused, inside delivery mode.
    expect(final.state.currentMode).toBe('delivery');
    expect(final.state.paused?.by).toBe('policy');
    expect(questionOf(final)).toMatch(DELIVERY_HOLD);

    // The events record approvals and requests for every contractual kind.
    const events = harness.repository.loadProject().events;
    const requested = events.filter((event) => event.type === 'artifact.approval.requested');
    const approved = events.filter((event) => event.type === 'artifact.approved');
    expect(requested).toHaveLength(5);
    expect(approved).toHaveLength(5);
    expect(final.state.verification.passed).toBe(true);

    // The fixture bug was actually fixed through the apply-patch tool.
    const greet = readFileSync(join(workspace, 'src/greet.js'), 'utf8');
    expect(greet).toContain('Hello, ${name}!');

    // No commits were made; only the intended file changed plus .relay state.
    const revCount = spawnSync('git', ['rev-list', '--count', '--all'], {
      cwd: workspace,
      encoding: 'utf8',
    });
    expect(revCount.stdout.trim()).toBe('0');
    const status = spawnSync('git', ['status', '--porcelain'], { cwd: workspace, encoding: 'utf8' });
    const lines = status.stdout.split('\n').filter((line) => line.length > 0);
    // The fixture files were staged at setup; the only worktree modification
    // is the intended greet.js patch, and .relay state is the only addition.
    const worktreeChanged = lines.filter((line) => line[1] === 'M' || line[1] === 'D');
    expect(worktreeChanged).toEqual(['AM src/greet.js']);
    expect(lines).toContain('?? .relay/');
  });

  it('completes the happy path when a harness approves each checkpoint (AC-2)', async () => {
    const harness = createHarness({
      provider: createWorkflowFakeProvider(),
      workspace: makeSampleWorkspace(),
    });
    await harness.controller.initialize(SAMPLE_GOAL);

    let approvals = 0;
    let result: StepResult | null = null;
    for (let index = 0; index < 12; index += 1) {
      result = await harness.controller.runUntilPause();
      const outcome = result.outcome;
      if (outcome.kind === 'NEEDS_USER' && DELIVERY_HOLD.test(outcome.question)) {
        break;
      }
      expect(result.outcome.kind).toBe('NEEDS_USER');
      await harness.controller.approve();
      approvals += 1;
    }
    expect(approvals).toBe(5);
    expect(result?.outcome.kind).toBe('NEEDS_USER');
    expect(result?.state.currentMode).toBe('delivery');
    expect(harness.controller.state.verification.passed).toBe(true);
    expect(harness.controller.statusView().nextAction).toMatch(DELIVERY_HOLD);
  });

  it('routes revisions back, marks downstream artifacts stale, and clears staleness on re-approval', async () => {
    const harness = createHarness({
      provider: createWorkflowFakeProvider(),
      workspace: makeSampleWorkspace(),
    });
    await harness.controller.initialize(SAMPLE_GOAL);
    await driveToDeliveryHold(harness.controller);

    const state = harness.controller.state;
    const prdId = state.artifactsByKind['prd'];
    const trdId = state.artifactsByKind['trd'];
    expect(prdId).toBeDefined();
    expect(trdId).toBeDefined();

    const revision = await harness.controller.handleInput('/revise prd tighten the greeting wording');
    expect(revision.outcome.kind).toBe('CONTINUE');
    expect(harness.controller.state.currentMode).toBe('specification');
    expect(harness.controller.state.paused).toBeNull(); // user mode entry clears the policy pause

    const step = await harness.controller.runStep();
    expect(step.outcome.kind).toBe('NEEDS_USER');
    const revised = harness.controller.state;
    expect(revised.artifacts[prdId as string]?.currentVersion).toBe(2);
    expect(revised.artifacts[prdId as string]?.status).toBe('draft');
    const downstreamKinds: readonly ArtifactKind[] = ['trd', 'execution-plan', 'verification-report', 'delivery-report'];
    for (const kind of downstreamKinds) {
      const id = revised.artifactsByKind[kind];
      expect(id, kind).toBeDefined();
      expect(revised.artifacts[id as string]?.status).toBe('stale');
      expect(revised.staleArtifacts).toContain(id);
    }

    // Approving the PRD advances back into technical-design to regenerate.
    const approval = await harness.controller.approve();
    expect(approval.outcome.kind).toBe('ADVANCE');
    expect(harness.controller.state.currentMode).toBe('technical-design');

    // Regenerating the TRD revises it; re-approval clears its staleness.
    await harness.controller.runStep();
    expect(harness.controller.state.artifacts[trdId as string]?.currentVersion).toBe(2);
    const trdApproval = await harness.controller.approve();
    expect(trdApproval.outcome.kind).toBe('ADVANCE');
    expect(harness.controller.state.artifacts[trdId as string]?.status).toBe('approved');
    expect(harness.controller.state.staleArtifacts).not.toContain(trdId);

    // The cascade completes and no artifact remains stale.
    const final = await driveToDeliveryHold(harness.controller);
    expect(final.state.currentMode).toBe('delivery');
    expect(final.state.staleArtifacts).toHaveLength(0);
    expect(final.state.artifacts[final.state.artifactsByKind['prd'] as string]?.currentVersion).toBe(2);
  });

  it('reconstructs identical state after a restart and resumes the interrupted workflow', async () => {
    const workspace = makeSampleWorkspace();
    const first = createHarness({ provider: createWorkflowFakeProvider(), workspace });
    await first.controller.initialize(SAMPLE_GOAL);

    // Pause mid-flow: after the PRD approval pause.
    const discovery = await first.controller.runStep();
    expect(discovery.outcome.kind).toBe('NEEDS_USER');
    await first.controller.approve();
    const specification = await first.controller.runStep();
    expect(specification.outcome.kind).toBe('NEEDS_USER');

    const stateBefore = first.controller.state;
    first.db.close();

    // Drop every handle; rebuild from the same workspace files.
    const second = createHarness({ provider: createWorkflowFakeProvider(), workspace });
    expect(second.controller.projectId).toBe(stateBefore.projectId);
    expect(second.controller.state).toEqual(stateBefore);

    const final = await driveToDeliveryHold(second.controller);
    expect(final.state.currentMode).toBe('delivery');
    expect(questionOf(final)).toMatch(DELIVERY_HOLD);
  });

  it('applies a revision requested in one process when the next step runs in a fresh process', async () => {
    const workspace = makeSampleWorkspace();
    const first = createHarness({ provider: createWorkflowFakeProvider(), workspace });
    await first.controller.initialize(SAMPLE_GOAL);
    await driveToDeliveryHold(first.controller);
    first.db.close();

    // A separate process issues the revision request (persisted intent only).
    const requester = createHarness({ provider: createWorkflowFakeProvider(), workspace });
    const routed = await requester.controller.handleInput('/revise prd scope down the requirements');
    expect(routed.outcome.kind).toBe('CONTINUE');
    expect(routed.state.currentMode).toBe('specification');
    requester.db.close();

    // A third process runs the next step: the approved PRD must be regenerated
    // as v2 (not skipped as "already complete"), and downstream work must be
    // marked stale pending re-approval.
    const worker = createHarness({ provider: createWorkflowFakeProvider(), workspace });
    const result = await worker.controller.runStep();
    expect(result.state.currentMode).toBe('specification');
    expect(result.outcome.kind).toBe('NEEDS_USER');
    expect(questionOf(result)).toMatch(/approve the prd \(v2\)/i);
    const prdId = result.state.artifactsByKind['prd'] as string;
    expect(result.state.artifacts[prdId]?.currentVersion).toBe(2);
    expect(result.state.artifacts[prdId]?.status).toBe('draft');
    const statuses = (kind: ArtifactKind): string | undefined =>
      result.state.artifacts[result.state.artifactsByKind[kind] as string]?.status;
    expect(statuses('trd')).toBe('stale');
    expect(statuses('execution-plan')).toBe('stale');
    expect(statuses('verification-report')).toBe('stale');
    expect(statuses('delivery-report')).toBe('stale');
    expect(statuses('opportunity-brief')).toBe('approved');
  });

  it('advances under autonomy off when the user approves a checkpoint or explicitly asks to advance', async () => {
    const workspace = makeSampleWorkspace();
    const harness = createHarness({ provider: createWorkflowFakeProvider(), workspace, initialAutonomy: 'off' });
    await harness.controller.initialize(SAMPLE_GOAL);

    // Discovery produces the brief and pauses for approval.
    const discovery = await harness.controller.runStep();
    expect(discovery.outcome.kind).toBe('NEEDS_USER');
    expect(discovery.state.currentMode).toBe('discovery');

    // The approval IS the user's authorization: the mode must actually change
    // even though autonomy is 'off' (which only restricts automatic advance).
    const approved = await harness.controller.approve();
    expect(approved.outcome.kind).toBe('ADVANCE');
    expect(approved.state.currentMode).toBe('specification');

    // An explicit natural-language advance also progresses instead of
    // returning an ADVANCE that never lands (the former livelock).
    const specification = await harness.controller.runStep();
    expect(specification.outcome.kind).toBe('NEEDS_USER'); // PRD approval pause
    await harness.controller.approve();
    expect(harness.controller.state.currentMode).toBe('technical-design');

    const advance = await harness.controller.handleInput('advance');
    expect(advance.outcome.kind).toBe('NEEDS_USER'); // TRD produced, approval pause
    expect(questionOf(advance)).toMatch(/approve the trd/i);
    await harness.controller.approve();
    expect(harness.controller.state.currentMode).toBe('planning');
  });

  it('pauses with a focused question on ambiguous natural-language input', async () => {
    const ambiguous = new FakeProvider([
      {
        match: { purpose: 'intent' },
        response: {
          structuredOutput: {
            kind: 'change-mode',
            confidence: 0.3,
            rationale: 'input could mean several things',
            mode: 'delivery',
          },
        },
      },
    ]);
    const harness = createHarness({ provider: ambiguous, workspace: makeSampleWorkspace() });
    await harness.controller.initialize(SAMPLE_GOAL);
    const eventsBefore = harness.controller.state.stats.events;

    const result = await harness.controller.handleInput('make it better maybe');
    expect(result.outcome.kind).toBe('NEEDS_USER');

    const state = harness.controller.state;
    expect(state.paused?.by).toBe('policy');
    expect(state.paused?.question).toBeTruthy();
    expect(state.paused?.question).toContain('delivery'); // names the top interpretation
    expect(state.currentMode).toBe('discovery');
    expect(Object.keys(state.artifacts)).toHaveLength(0);
    expect(state.transitions).toHaveLength(0);
    // Only classification telemetry + the intent proposal + the pause.
    expect(state.stats.events - eventsBefore).toBe(4);
    expect(state.stats.modelCalls).toBe(1);
  });

  it('leaves resumable state after an unrecoverable provider failure and continues with a working provider', async () => {
    const workspace = makeSampleWorkspace();
    const failing = new FakeProvider([
      ...sampleWorkflowSteps().filter(
        (step) => step.match.purpose !== 'mode:technical-design' && step.match.purpose !== 'intent',
      ),
      {
        match: { purpose: 'mode:technical-design' },
        response: {
          error: { kind: 'server', retryable: true, message: 'injected server failure', failTimes: 5 },
        },
      },
    ]);
    const harness = createHarness({ provider: failing, workspace });
    await harness.controller.initialize(SAMPLE_GOAL);

    await harness.controller.runStep(); // discovery
    await harness.controller.approve();
    await harness.controller.runStep(); // specification
    await harness.controller.approve();
    expect(harness.controller.state.currentMode).toBe('technical-design');

    const failure = await harness.controller.runStep();
    expect(failure.outcome.kind).toBe('NEEDS_USER');
    expect(questionOf(failure)).toContain('injected server failure');
    expect(harness.controller.state.paused?.by).toBe('policy');
    const events = harness.repository.loadProject().events;
    expect(events.some((event) => event.type === 'model.failed')).toBe(true);

    // Replace the provider (fresh controller on the same persisted state).
    harness.db.close();
    const recovered = createHarness({ provider: createWorkflowFakeProvider(), workspace });
    expect(recovered.controller.state.paused).not.toBeNull();
    await recovered.controller.resume();
    const final = await driveToDeliveryHold(recovered.controller);
    expect(final.state.currentMode).toBe('delivery');
  });

  it('gives verification an independent context: criteria, plan, diff, and test evidence only', async () => {
    const inner = createWorkflowFakeProvider();
    const requests: ModelRequest[] = [];
    const recording: ModelProvider = {
      name: 'fake',
      models: ['fake-standard'],
      stream(request: ModelRequest) {
        requests.push(request);
        return inner.stream(request);
      },
    };
    const harness = createHarness({ provider: recording, workspace: makeSampleWorkspace() });
    await harness.controller.initialize(SAMPLE_GOAL);

    // Drive to the verification mode (entered after implementation completes).
    for (let index = 0; index < 6; index += 1) {
      const step = await harness.controller.runStep();
      if (step.outcome.kind === 'NEEDS_USER') {
        await harness.controller.approve();
      }
      if (harness.controller.state.currentMode === 'verification') break;
    }
    expect(harness.controller.state.currentMode).toBe('verification');

    requests.length = 0;
    const verification = await harness.controller.runStep();
    expect(verification.outcome.kind).toBe('NEEDS_USER'); // approval pause for the report

    const verificationRequests = requests.filter((request) => request.purpose === 'mode:verification');
    expect(verificationRequests.length).toBeGreaterThanOrEqual(1);
    const packet = verificationRequests[verificationRequests.length - 1]?.messages[0];
    expect(packet?.role).toBe('user');
    const content = packet?.role === 'user' ? packet.content : '';
    expect(content).toContain('Acceptance criteria');
    expect(content).toContain('AC-1');
    expect(content).toContain('Hello, ${name}!'); // the actual diff
    expect(content).toContain('DECISION-LOG-EVIDENCE'); // test evidence from the decision log
    // The implementation conversation and mode contract never leak in.
    expect(content).not.toContain('Execute approved, in-scope tasks');
    expect(content).not.toContain('Relay task context — implementation');
  });

  it('pauses with the budget message when the model-call budget is exhausted, and resumes', async () => {
    const workspace = makeSampleWorkspace();
    const oneCall = new FakeProvider([
      {
        match: { purpose: 'mode:discovery' },
        response: {
          toolCalls: [{ name: 'read-file', args: { path: 'README.md' } }],
          finishReason: 'tool-calls',
        },
      },
    ]);
    const harness = createHarness({ provider: oneCall, workspace, budgets: { maxModelCalls: 1 } });
    await harness.controller.initialize(SAMPLE_GOAL);

    const exhausted = await harness.controller.runStep();
    expect(exhausted.outcome.kind).toBe('NEEDS_USER');
    expect(exhausted.state.paused?.by).toBe('policy');
    expect(exhausted.state.paused?.reason).toContain('maxModelCalls');
    const events = harness.repository.loadProject().events;
    expect(events.some((event) => event.type === 'budget.exhausted')).toBe(true);

    // State is resumable with a working provider and normal budgets.
    harness.db.close();
    const recovered = createHarness({ provider: createWorkflowFakeProvider(), workspace });
    await recovered.controller.resume();
    const next = await recovered.controller.runStep();
    expect(next.outcome.kind).toBe('NEEDS_USER');
    expect(questionOf(next)).toContain('Approve the opportunity-brief');
  });

  it('carries the demo goal through the whole pipeline with the scripted demo provider', async () => {
    const harness = createHarness({ provider: createDemoFakeProvider(), workspace: makeBareWorkspace() });
    await harness.controller.initialize(DEMO_GOAL);
    const final = await driveToDeliveryHold(harness.controller);

    expect(final.state.currentMode).toBe('delivery');
    expect(questionOf(final)).toMatch(DELIVERY_HOLD);
    for (const kind of ARTIFACT_KINDS) {
      const stored = await harness.artifacts.list(kind);
      expect(stored.length, kind).toBe(1);
    }
    const brief = await harness.artifacts.latest('opportunity-brief');
    expect(brief?.title).toMatch(/demo/i);
    expect(final.state.goal).toBe(DEMO_GOAL);
  });

  it('supports the command surface and natural-language advance', async () => {
    const harness = createHarness({
      provider: createWorkflowFakeProvider(),
      workspace: makeSampleWorkspace(),
    });
    await harness.controller.initialize(SAMPLE_GOAL);

    expect((await harness.controller.handleInput('/status')).outcome.kind).toBe('CONTINUE');
    expect((await harness.controller.handleInput('/why')).outcome.kind).toBe('CONTINUE');
    expect((await harness.controller.handleInput('/auto off')).outcome.kind).toBe('CONTINUE');
    expect(harness.controller.state.autonomy).toBe('off');

    const paused = await harness.controller.handleInput('/pause holding for review');
    expect(paused.outcome.kind).toBe('NEEDS_USER');
    expect(harness.controller.state.paused?.by).toBe('user');

    // A mode change while paused is rejected by policy and recorded.
    const rejected = await harness.controller.handleInput('/mode specification');
    expect(rejected.outcome.kind).toBe('NEEDS_USER');
    expect(harness.controller.state.rejectedTransitions).toHaveLength(1);
    expect(harness.controller.state.currentMode).toBe('discovery');

    expect((await harness.controller.handleInput('/resume')).outcome.kind).toBe('CONTINUE');
    expect((await harness.controller.handleInput('/bogus')).outcome.kind).toBe('NEEDS_USER');
    expect((await harness.controller.handleInput('')).outcome.kind).toBe('NEEDS_USER');

    // Commands produced validated intent proposals and applied intents.
    const events = harness.repository.loadProject().events;
    const proposed = events.filter((event) => event.type === 'intent.proposed');
    const applied = events.filter((event) => event.type === 'intent.applied');
    expect(proposed.length).toBeGreaterThanOrEqual(6);
    expect(applied.length).toBeGreaterThanOrEqual(5);
    expect(applied.every((event) => event.type === 'intent.applied' && event.payload.source === 'command')).toBe(true);

    // Natural language: the scripted intent classifies 'continue' as advance.
    const advanced = await harness.controller.handleInput('continue');
    expect(advanced.outcome.kind).toBe('NEEDS_USER');
    expect(questionOf(advanced)).toContain('Approve the opportunity-brief');
  });

  it('guards initialization: uninitialized access throws, double init is rejected', async () => {
    const harness = createHarness({
      provider: createWorkflowFakeProvider(),
      workspace: makeSampleWorkspace(),
    });
    expect(harness.controller.projectId).toBeNull();
    expect(() => harness.controller.state).toThrow(NotInitializedError);
    expect(() => harness.controller.statusView()).toThrow(NotInitializedError);
    expect((await harness.controller.handleInput('/status')).outcome.kind).toBe('NEEDS_USER');

    await harness.controller.initialize(SAMPLE_GOAL);
    expect(harness.controller.projectId).not.toBeNull();
    await expect(harness.controller.initialize('another goal')).rejects.toThrow(ProjectAlreadyInitializedError);
  });
});
