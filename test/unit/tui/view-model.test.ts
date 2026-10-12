/**
 * View model folding tests: event-to-entry mapping, transcript capping,
 * outcome labeling, and UI action handling. No React, no terminal.
 */

import { describe, expect, it } from 'vitest';
import {
  createViewModel,
  readControllerState,
  TRANSCRIPT_CAP,
  type ControllerUiEvent,
  type TuiController,
  type TuiViewModel,
} from '../../../src/tui/view-model.js';

function fold(events: readonly ControllerUiEvent[]): TuiViewModel {
  const model = createViewModel();
  for (const event of events) {
    model.dispatch({ type: 'controller-event', event });
  }
  return model.getState();
}

describe('createViewModel controller events', () => {
  it('merges consecutive deltas into a single assistant entry', () => {
    const state = fold([
      { kind: 'delta', text: 'Hello' },
      { kind: 'delta', text: ' ' },
      { kind: 'delta', text: 'world' },
    ]);
    expect(state.transcript).toHaveLength(1);
    expect(state.transcript[0]).toEqual({ kind: 'assistant', text: 'Hello world' });
  });

  it('starts a new assistant entry after another entry kind', () => {
    const state = fold([
      { kind: 'delta', text: 'first' },
      { kind: 'notice', message: 'saved' },
      { kind: 'delta', text: 'second' },
    ]);
    expect(state.transcript.map((entry) => entry.kind)).toEqual(['assistant', 'notice', 'assistant']);
  });

  it('folds mode, tool, artifact, notice, and error events into prefixed entries', () => {
    const state = fold([
      { kind: 'mode', mode: 'specification', previous: 'discovery', reason: 'brief approved' },
      { kind: 'tool', tool: 'read-file', capability: 'read', phase: 'start' },
      { kind: 'tool', tool: 'read-file', capability: 'read', phase: 'end', ok: true, summary: 'CLAUDE.md' },
      { kind: 'tool', tool: 'run-command', capability: 'execute', phase: 'end', ok: false, summary: 'exit 1' },
      {
        kind: 'artifact',
        artifactId: 'art_1',
        artifactKind: 'prd',
        version: 2,
        status: 'approved',
      },
      { kind: 'notice', message: 'persisted' },
      { kind: 'error', message: 'provider unavailable' },
    ]);
    expect(state.transcript).toEqual([
      { kind: 'mode', mode: 'specification', previous: 'discovery', reason: 'brief approved' },
      { kind: 'tool', tool: 'read-file', phase: 'start' },
      { kind: 'tool', tool: 'read-file', phase: 'end', ok: true, summary: 'CLAUDE.md' },
      { kind: 'tool', tool: 'run-command', phase: 'end', ok: false, summary: 'exit 1' },
      { kind: 'artifact', artifactId: 'art_1', artifactKind: 'prd', version: 2, status: 'approved' },
      { kind: 'notice', message: 'persisted' },
      { kind: 'error', message: 'provider unavailable' },
    ]);
    expect(state.lastError).toBe('provider unavailable');
  });

  it('labels outcome events with the four documented kinds', () => {
    const state = fold([
      { kind: 'outcome', outcome: { kind: 'CONTINUE', reason: 'keep drafting' } },
      { kind: 'outcome', outcome: { kind: 'ADVANCE', to: 'specification', reason: 'brief approved' } },
      { kind: 'outcome', outcome: { kind: 'NEEDS_USER', question: 'approve?', options: ['yes', 'no'], reason: 'contract approval' } },
      { kind: 'outcome', outcome: { kind: 'REVISE', stage: 'discovery', reason: 'stale brief' } },
    ]);
    const labels = state.transcript.map((entry) => (entry.kind === 'outcome' ? entry.label : ''));
    expect(labels).toEqual(['CONTINUE', 'ADVANCE', 'NEEDS_USER', 'REVISE']);
    const reasons = state.transcript.map((entry) => (entry.kind === 'outcome' ? entry.reason : ''));
    expect(reasons).toEqual(['keep drafting', 'brief approved', 'contract approval', 'stale brief']);
  });

  it('falls back to NEEDS_USER for unrecognized outcome payloads', () => {
    const malformed = [
      { kind: 'outcome', outcome: undefined },
      { kind: 'outcome', outcome: 'gibberish' },
    ] as unknown as readonly ControllerUiEvent[];
    const state = fold(malformed);
    for (const entry of state.transcript) {
      expect(entry.kind).toBe('outcome');
      if (entry.kind === 'outcome') expect(entry.label).toBe('NEEDS_USER');
    }
  });

  it('ignores session events, which carry no transcript text', () => {
    const state = fold([{ kind: 'session', sessionId: 's_1' }]);
    expect(state.transcript).toHaveLength(0);
  });

  it('caps the transcript at the last 500 entries', () => {
    const model = createViewModel();
    for (let i = 0; i < TRANSCRIPT_CAP + 60; i += 1) {
      model.dispatch({ type: 'controller-event', event: { kind: 'notice', message: `n${i}` } });
    }
    const state = model.getState();
    expect(state.transcript).toHaveLength(TRANSCRIPT_CAP);
    expect(state.transcript[0]).toEqual({ kind: 'notice', message: 'n60' });
    expect(state.transcript.at(-1)).toEqual({ kind: 'notice', message: `n${TRANSCRIPT_CAP + 59}` });
  });

  it('does not mutate previously returned snapshots', () => {
    const model = createViewModel();
    model.dispatch({ type: 'controller-event', event: { kind: 'delta', text: 'one' } });
    const before = model.getState();
    model.dispatch({ type: 'controller-event', event: { kind: 'delta', text: 'two' } });
    const after = model.getState();
    expect(before.transcript).toHaveLength(1);
    expect(after.transcript).toHaveLength(1);
    expect(before.transcript[0]).toEqual({ kind: 'assistant', text: 'one' });
    expect(after.transcript[0]).toEqual({ kind: 'assistant', text: 'onetwo' });
  });
});

describe('readControllerState', () => {
  it('returns the empty projection when the controller is not initialized', () => {
    const uninitialized: TuiController = {
      projectId: null,
      get state(): never {
        throw new Error('No Relay project is initialized');
      },
      subscribe: () => () => undefined,
      handleInput: async () => undefined,
      runStep: async () => undefined,
      initialize: async () => undefined,
      statusView: () => {
        throw new Error('No Relay project is initialized');
      },
      whyView: () => {
        throw new Error('No Relay project is initialized');
      },
      inspect: async () => null,
    };
    expect(readControllerState(uninitialized).projectId).toBe('');
    expect(readControllerState(uninitialized).currentMode).toBe('discovery');
  });
});

describe('createViewModel actions', () => {
  it('records trimmed user input and skips empty input', () => {
    const model = createViewModel();
    model.dispatch({ type: 'user-input', text: '  build a timer  ' });
    model.dispatch({ type: 'user-input', text: '   ' });
    expect(model.getState().transcript).toEqual([{ kind: 'user', text: 'build a timer' }]);
  });

  it('sets the panel and its rendered content', () => {
    const model = createViewModel();
    model.dispatch({ type: 'panel', panel: 'status', content: 'goal: ship it' });
    expect(model.getState().panel).toBe('status');
    expect(model.getState().panelContent).toBe('goal: ship it');
    model.dispatch({ type: 'panel', panel: 'none' });
    expect(model.getState().panel).toBe('none');
    expect(model.getState().panelContent).toBe('');
  });

  it('toggles busy and exit flags', () => {
    const model = createViewModel();
    model.dispatch({ type: 'busy', busy: true });
    expect(model.getState().busy).toBe(true);
    model.dispatch({ type: 'busy', busy: false });
    expect(model.getState().busy).toBe(false);
    expect(model.getState().exitRequested).toBe(false);
    model.dispatch({ type: 'exit' });
    expect(model.getState().exitRequested).toBe(true);
  });

  it('shows error messages in the transcript and clears the flag with null', () => {
    const model = createViewModel();
    model.dispatch({ type: 'error', message: 'route failed' });
    const errored = model.getState();
    expect(errored.lastError).toBe('route failed');
    expect(errored.transcript.at(-1)).toEqual({ kind: 'error', message: 'route failed' });
    model.dispatch({ type: 'error', message: null });
    expect(model.getState().lastError).toBeNull();
    expect(model.getState().transcript).toHaveLength(1);
  });

  it('tracks terminal width and clamps degenerate values', () => {
    const model = createViewModel();
    expect(model.getState().columns).toBe(80);
    model.dispatch({ type: 'resize', columns: 40 });
    expect(model.getState().columns).toBe(40);
    model.dispatch({ type: 'resize', columns: Number.NaN });
    expect(model.getState().columns).toBe(80);
    model.dispatch({ type: 'resize', columns: 1 });
    expect(model.getState().columns).toBe(20);
  });
});
