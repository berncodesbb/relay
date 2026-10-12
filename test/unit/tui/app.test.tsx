/**
 * Interactive app rendering tests (ink-testing-library) plus runInteractive
 * lifecycle tests against fake streams: header, pause overlay, panels, prompt
 * editing, slash routing, initial message bootstrap, ctrl-c semantics,
 * NO_COLOR and narrow-width rendering, and terminal restoration.
 */

import { EventEmitter } from 'node:events';
import { afterEach, describe, expect, it } from 'vitest';
import { render } from 'ink-testing-library';
import { createEmptyProjectState, type ProjectState } from '../../../src/core/project-state.js';
import type { WorkflowController } from '../../../src/runtime/controller.js';
import { App, runInteractive } from '../../../src/tui/app.js';
import type { TuiController } from '../../../src/tui/view-model.js';
import { createStubController, type StubController } from './stub-controller.js';

// Compile-time contract check: the real controller must satisfy the TUI's
// structural port without adaptation.
type ControllerSatisfiesPort = WorkflowController extends TuiController ? true : false;
const controllerSatisfiesPort: ControllerSatisfiesPort = true;
void controllerSatisfiesPort;

const settle = (ms = 60): Promise<void> => new Promise((resolve) => { setTimeout(resolve, ms); });

function baseState(overrides: Partial<ProjectState> = {}): ProjectState {
  return {
    ...createEmptyProjectState(),
    projectId: 'proj_1',
    goal: 'ship a sailing weather CLI',
    currentMode: 'discovery',
    ...overrides,
  };
}

const noopExit = (): void => {};

let unmounts: Array<() => void> = [];

function mountApp(stub: StubController, props: { [key: string]: unknown } = {}) {
  const instance = render(<App controller={stub} onExit={noopExit} {...props} />);
  unmounts.push(instance.unmount);
  return instance;
}

afterEach(() => {
  for (const unmount of unmounts) unmount();
  unmounts = [];
});

describe('App rendering', () => {
  it('shows mode, stage, and autonomy in the header and updates after a mode event', async () => {
    const stub = createStubController({ projectId: 'proj_1', state: baseState() });
    const { lastFrame } = mountApp(stub);
    await settle();
    expect(lastFrame()).toContain('relay — Discovery [1/7]');
    expect(lastFrame()).toContain('auto:next-checkpoint');
    expect(lastFrame()).toContain('ship a sailing weather CLI');

    stub.setState(baseState({ currentMode: 'specification' }));
    stub.emit({
      kind: 'mode',
      mode: 'specification',
      previous: 'discovery',
      reason: 'brief approved',
    });
    await settle();
    expect(lastFrame()).toContain('Specification [2/7]');
    expect(lastFrame()).toContain('discovery -> specification');
  });

  it('shows the pause overlay with question, options, and concrete commands', async () => {
    const stub = createStubController({
      projectId: 'proj_1',
      state: baseState({
        paused: {
          by: 'policy',
          reason: 'PRD is a contractual artifact',
          question: 'Approve the PRD as the contract?',
          options: ['approve it', 'request changes'],
          at: '2026-10-12T10:00:00.000Z',
        },
      }),
    });
    const { lastFrame } = mountApp(stub);
    await settle();
    const frame = lastFrame() ?? '';
    expect(frame).toContain('relay needs your decision');
    expect(frame).toContain('PRD is a contractual artifact');
    expect(frame).toContain('Approve the PRD as the contract?');
    expect(frame).toContain('1) approve it');
    expect(frame).toContain('2) request changes');
    expect(frame).toContain('/approve');
    expect(frame).toContain('/revise prd <why>');
    expect(frame).toContain('/auto workflow');
    expect(frame).toContain('/why');
  });

  it('shows a pause overlay for a NEEDS_USER outcome and clears it once the user answers', async () => {
    const stub = createStubController({
      projectId: 'proj_1',
      state: baseState({
        lastOutcome: {
          at: '2026-10-12T11:00:00.000Z',
          outcome: 'NEEDS_USER',
          to: null,
          reviseStage: null,
          reason: 'delivery requires confirmation',
          evidence: [],
        },
      }),
    });
    const { lastFrame, stdin } = mountApp(stub);
    await settle();
    expect(lastFrame()).toContain('relay needs your decision');
    expect(lastFrame()).toContain('delivery requires confirmation');

    stdin.write('please continue');
    await settle();
    stdin.write('\r');
    await settle();
    expect(lastFrame()).not.toContain('relay needs your decision');
    expect(stub.handleInputCalls).toEqual(['please continue']);
  });

  it('opens the command reference on /help', async () => {
    const stub = createStubController({ projectId: 'proj_1', state: baseState() });
    const { lastFrame, stdin } = mountApp(stub);
    await settle();
    stdin.write('/help');
    await settle(20);
    stdin.write('\r');
    await settle();
    const frame = lastFrame() ?? '';
    expect(frame).toContain('help');
    expect(frame).toContain('/inspect <artifact>');
    expect(frame).toContain('/revise <kind> <why>');
  });

  it('opens the status panel with the goal text on /status', async () => {
    const stub = createStubController({ projectId: 'proj_1', state: baseState() });
    const { lastFrame, stdin } = mountApp(stub);
    await settle();
    stdin.write('/status');
    await settle(20);
    stdin.write('\r');
    await settle();
    const frame = lastFrame() ?? '';
    expect(frame).toContain('Goal: ship a sailing weather CLI');
    expect(frame).toContain('Mode: discovery');
    expect(frame).toContain('Next Action: draft the opportunity brief');
  });

  it('opens the why panel on /why and the inspect panel on /inspect', async () => {
    const stub = createStubController({
      projectId: 'proj_1',
      state: baseState({
        lastOutcome: {
          at: '2026-10-12T12:00:00.000Z',
          outcome: 'ADVANCE',
          to: 'specification',
          reviseStage: null,
          reason: 'brief approved with evidence',
          evidence: ['opportunity-brief v1 approved'],
        },
      }),
    });
    const { lastFrame, stdin } = mountApp(stub);
    await settle();
    stdin.write('/why');
    await settle(20);
    stdin.write('\r');
    await settle();
    expect(lastFrame()).toContain('brief approved with evidence');
    expect(lastFrame()).toContain('opportunity-brief v1 approved');

    stdin.write('/inspect prd');
    await settle(20);
    stdin.write('\r');
    await settle();
    expect(stub.inspectCalls).toEqual(['prd']);
    expect(lastFrame()).toContain('Opportunity Brief (stub)');
  });

  it('surfaces inspect failures as transcript errors', async () => {
    const stub = createStubController({
      projectId: 'proj_1',
      state: baseState(),
      inspectError: new Error('no such artifact'),
    });
    const { lastFrame, stdin } = mountApp(stub);
    await settle();
    stdin.write('/inspect missing');
    await settle(20);
    stdin.write('\r');
    await settle();
    expect(lastFrame()).toContain('no such artifact');
  });

  it('sends typed input verbatim on enter', async () => {
    const stub = createStubController({ projectId: 'proj_1', state: baseState() });
    const { lastFrame, stdin } = mountApp(stub);
    await settle();
    stdin.write('build a markdown timer');
    await settle(20);
    stdin.write('\r');
    await settle();
    expect(stub.handleInputCalls).toEqual(['build a markdown timer']);
    expect(lastFrame()).toContain('you> build a markdown timer');
  });

  it('edits the prompt line: append, backspace, history recall', async () => {
    const stub = createStubController({ projectId: 'proj_1', state: baseState() });
    const { lastFrame, stdin } = mountApp(stub);
    await settle();
    stdin.write('hell');
    await settle(20);
    stdin.write('o');
    await settle(20);
    expect(lastFrame()).toContain('› hello');

    stdin.write('\u007f');
    await settle(20);
    expect(lastFrame()).toContain('› hell');
    expect(lastFrame()).not.toContain('› hello');

    stdin.write('\r');
    await settle(20);
    expect(lastFrame()).toContain('you> hell');
    stdin.write('\u001b[A');
    await settle(20);
    expect(lastFrame()).toContain('› hell');
  });

  it('shows working… while a controller operation is in flight', async () => {
    let release: () => void = noopExit;
    const gate = new Promise<void>((resolve) => { release = resolve; });
    const stub = createStubController({
      projectId: 'proj_1',
      state: baseState(),
      handleInputImpl: async () => { await gate; },
    });
    const { lastFrame, stdin } = mountApp(stub);
    await settle();
    stdin.write('take your time');
    await settle(20);
    stdin.write('\r');
    await settle();
    expect(lastFrame()).toContain('working…');
    expect(lastFrame()).toContain('you> take your time');

    release();
    await settle();
    expect(lastFrame()).not.toContain('working…');
  });

  it('renders transcript entries with their stable prefixes', async () => {
    const stub = createStubController({ projectId: 'proj_1', state: baseState() });
    const { lastFrame } = mountApp(stub);
    await settle();
    stub.emit({ kind: 'delta', text: 'Drafting the brief.' });
    stub.emit({ kind: 'tool', tool: 'read-file', capability: 'read', phase: 'start' });
    stub.emit({ kind: 'tool', tool: 'read-file', capability: 'read', phase: 'end', ok: true, summary: 'CLAUDE.md' });
    stub.emit({ kind: 'tool', tool: 'run-command', capability: 'execute', phase: 'end', ok: false, summary: 'exit 1' });
    stub.emit({
      kind: 'artifact',
      artifactId: 'art_9',
      artifactKind: 'prd',
      version: 2,
      status: 'approved',
    });
    stub.emit({
      kind: 'outcome',
      outcome: { kind: 'ADVANCE', to: 'specification', reason: 'brief approved' },
    });
    await settle();
    const frame = lastFrame() ?? '';
    expect(frame).toContain('relay> Drafting the brief.');
    expect(frame).toContain('tool+ read-file');
    expect(frame).toContain('tool✔ read-file — CLAUDE.md');
    expect(frame).toContain('tool✘ run-command — exit 1');
    expect(frame).toContain('art prd v2 approved (art_9)');
    expect(frame).toContain('out ADVANCE — brief approved');
  });

  it('initializes and runs one step when uninitialized with an initial message', async () => {
    const stub = createStubController();
    mountApp(stub, { initialMessage: 'ship a sailing weather CLI' });
    await settle();
    expect(stub.initializeCalls).toEqual(['ship a sailing weather CLI']);
    expect(stub.runStepCalls).toBe(1);
    expect(stub.handleInputCalls).toEqual([]);
  });

  it('mounts on an uninitialized controller and reports /status instead of crashing', async () => {
    const stub = createStubController();
    const { lastFrame, stdin } = mountApp(stub);
    await settle();
    expect(lastFrame()).toContain('no project yet');
    stdin.write('/status');
    await settle(20);
    stdin.write('\r');
    await settle();
    expect(lastFrame()).toContain('No Relay project is initialized');
  });

  it('treats an initial message as input for an existing project', async () => {
    const stub = createStubController({ projectId: 'proj_1', state: baseState() });
    mountApp(stub, { initialMessage: 'add tide tables' });
    await settle();
    expect(stub.handleInputCalls).toEqual(['add tide tables']);
    expect(stub.initializeCalls).toEqual([]);
    expect(stub.runStepCalls).toBe(0);
  });

  it('renders under NO_COLOR without breaking', async () => {
    const previous = process.env.NO_COLOR;
    process.env.NO_COLOR = '1';
    try {
      const stub = createStubController({ projectId: 'proj_1', state: baseState() });
      const { lastFrame } = mountApp(stub);
      await settle();
      const frame = lastFrame() ?? '';
      expect(frame.length).toBeGreaterThan(0);
      expect(frame).toContain('relay — Discovery [1/7]');
    } finally {
      if (previous === undefined) delete process.env.NO_COLOR;
      else process.env.NO_COLOR = previous;
    }
  });

  it('truncates instead of crashing at 40 columns', async () => {
    const goal = 'a'.repeat(80);
    const stub = createStubController({
      projectId: 'proj_1',
      state: baseState({ goal }),
    });
    const { lastFrame } = mountApp(stub, { fixedColumns: 40 });
    await settle();
    const frame = lastFrame() ?? '';
    expect(frame).toContain('relay — Discovery [1/7]');
    expect(frame).toContain(`${'a'.repeat(35)}…`);
    expect(frame).not.toContain('a'.repeat(80));
  });
});

class FakeStdout extends EventEmitter {
  readonly frames: string[] = [];
  columns = 80;
  isTTY = false;
  destroyed = false;
  writableEnded = false;
  writable = true;
  write = (data: string): boolean => {
    this.frames.push(data);
    return true;
  };
}

class FakeStdin extends EventEmitter {
  isTTY = true;
  data: string | null = null;
  readonly rawModes: boolean[] = [];
  write = (chunk: string): void => {
    this.data = chunk;
    this.emit('readable');
  };
  read = (): string | null => {
    const chunk = this.data;
    this.data = null;
    return chunk;
  };
  setRawMode(mode: boolean): this {
    this.rawModes.push(mode);
    return this;
  }
  setEncoding(): void {}
  resume(): void {}
  pause(): void {}
  ref(): void {}
  unref(): void {}
}

const asReadable = (stream: FakeStdin): NodeJS.ReadableStream => stream as unknown as NodeJS.ReadableStream;
const asWritable = (stream: FakeStdout): NodeJS.WritableStream => stream as unknown as NodeJS.WritableStream;

describe('runInteractive lifecycle', () => {
  it('exits 0 on /quit and restores raw mode', async () => {
    const stub = createStubController({ projectId: 'proj_1', state: baseState() });
    const stdin = new FakeStdin();
    const stdout = new FakeStdout();
    const promise = runInteractive({
      controller: stub,
      signal: new AbortController().signal,
      stdin: asReadable(stdin),
      stdout: asWritable(stdout),
    });
    await settle();
    stdin.write('/quit');
    await settle(20);
    stdin.write('\r');
    const result = await promise;
    expect(result).toEqual({ exitCode: 0 });
    expect(stdin.rawModes[0]).toBe(true);
    expect(stdin.rawModes.at(-1)).toBe(false);
  });

  it('exits 0 on ctrl-c when idle', async () => {
    const stub = createStubController({ projectId: 'proj_1', state: baseState() });
    const stdin = new FakeStdin();
    const stdout = new FakeStdout();
    const promise = runInteractive({
      controller: stub,
      signal: new AbortController().signal,
      stdin: asReadable(stdin),
      stdout: asWritable(stdout),
    });
    await settle();
    stdin.write('\x03');
    const result = await promise;
    expect(result).toEqual({ exitCode: 0 });
  });

  it('cancels on the first ctrl-c while busy and force-exits 130 on the second', async () => {
    const never = new Promise<void>(() => {});
    let cancelled = 0;
    const stub = createStubController({
      projectId: 'proj_1',
      state: baseState(),
      handleInputImpl: async () => { await never; },
    });
    const stdin = new FakeStdin();
    const stdout = new FakeStdout();
    const promise = runInteractive({
      controller: stub,
      signal: new AbortController().signal,
      stdin: asReadable(stdin),
      stdout: asWritable(stdout),
      requestCancel: () => { cancelled += 1; },
    });
    await settle();
    stdin.write('slow work');
    await settle(20);
    stdin.write('\r');
    await settle();
    expect(stdout.frames.join('')).toContain('working…');

    stdin.write('\x03');
    await settle();
    expect(cancelled).toBe(1);
    expect(stdout.frames.join('')).toContain('cancel requested');

    stdin.write('\x03');
    const result = await promise;
    expect(result).toEqual({ exitCode: 130 });
  });

  it('unmounts with 130 when the abort signal fires', async () => {
    const stub = createStubController({ projectId: 'proj_1', state: baseState() });
    const stdin = new FakeStdin();
    const stdout = new FakeStdout();
    const abort = new AbortController();
    const promise = runInteractive({ controller: stub, signal: abort.signal, stdin: asReadable(stdin), stdout: asWritable(stdout) });
    await settle();
    abort.abort();
    const result = await promise;
    expect(result).toEqual({ exitCode: 130 });
    expect(stdin.rawModes.at(-1)).toBe(false);
  });

  it('returns 130 without rendering when the signal is already aborted', async () => {
    const stub = createStubController({ projectId: 'proj_1', state: baseState() });
    const abort = new AbortController();
    abort.abort();
    const result = await runInteractive({
      controller: stub,
      signal: abort.signal,
      stdin: asReadable(new FakeStdin()),
      stdout: asWritable(new FakeStdout()),
    });
    expect(result).toEqual({ exitCode: 130 });
  });

  it('prints the session-ended line only for TTY stdout', async () => {
    const ttyStub = createStubController({ projectId: 'proj_1', state: baseState() });
    const ttyStdin = new FakeStdin();
    const ttyStdout = new FakeStdout();
    ttyStdout.isTTY = true;
    const ttyPromise = runInteractive({
      controller: ttyStub,
      signal: new AbortController().signal,
      stdin: asReadable(ttyStdin),
      stdout: asWritable(ttyStdout),
    });
    await settle();
    ttyStdin.write('/quit');
    await settle(20);
    ttyStdin.write('\r');
    await ttyPromise;
    expect(ttyStdout.frames.join('')).toContain('relay: session ended — run relay status anytime');

    const pipeStub = createStubController({ projectId: 'proj_1', state: baseState() });
    const pipeStdin = new FakeStdin();
    const pipeStdout = new FakeStdout();
    const pipePromise = runInteractive({
      controller: pipeStub,
      signal: new AbortController().signal,
      stdin: asReadable(pipeStdin),
      stdout: asWritable(pipeStdout),
    });
    await settle();
    pipeStdin.write('/quit');
    await settle(20);
    pipeStdin.write('\r');
    await pipePromise;
    expect(pipeStdout.frames.join('')).not.toContain('session ended');
  });
});
