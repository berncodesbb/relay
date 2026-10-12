/**
 * Event-driven view model for the interactive terminal interface.
 *
 * The view model is a pure, React-free fold over controller UI events and
 * local UI actions. Components never orchestrate workflow work; they render
 * {@link TuiViewModel} snapshots and dispatch {@link TuiAction}s. This keeps
 * folding deterministic and unit-testable without a terminal.
 *
 * `ControllerUiEvent` is the real controller event union from
 * `src/runtime/controller.ts`; folding still reads payloads defensively so a
 * malformed or drifted event degrades gracefully instead of crashing the
 * interface.
 */

import { createEmptyProjectState, type ProjectState } from '../core/project-state.js';
import type { ControllerUiEvent } from '../runtime/controller.js';
import type { StatusView, WhyView } from '../runtime/controller-views.js';

export type { ControllerUiEvent } from '../runtime/controller.js';

/**
 * Structural port of the workflow controller's public surface, exactly as the
 * terminal interface consumes it. Only the members the TUI actually calls are
 * declared so minor additions to the controller cannot break compilation here.
 * View payloads are still rendered defensively (see `renderViewText` in
 * `app.tsx`), and every member may throw `NotInitializedError` before a
 * project exists — reads go through {@link readControllerState}.
 */
export type TuiController = {
  /** Project id, or null when no project is initialized yet. */
  readonly projectId?: string | null | undefined;
  /** Synchronous projection of the persisted event log. Throws when uninitialized. */
  readonly state: ProjectState;
  subscribe(listener: (event: ControllerUiEvent) => void): () => void;
  handleInput(input: string): Promise<unknown>;
  runStep(): Promise<unknown>;
  initialize(goal: string): Promise<unknown>;
  statusView(): StatusView;
  whyView(): WhyView;
  inspect(target: string): Promise<unknown>;
};

/**
 * Reads the controller state snapshot, falling back to the empty projection
 * when no project is initialized yet (the controller throws in that case).
 */
export function readControllerState(controller: TuiController): ProjectState {
  try {
    return controller.state;
  } catch {
    return createEmptyProjectState();
  }
}

/** One rendered line of conversation or activity. */
export type TranscriptEntry =
  | { kind: 'user'; text: string }
  | { kind: 'assistant'; text: string }
  | { kind: 'tool'; tool: string; phase: 'start' | 'end'; ok?: boolean; summary?: string }
  | { kind: 'artifact'; artifactId: string; artifactKind: string; version: number; status: string }
  | { kind: 'mode'; mode: string; previous: string | null; reason: string }
  | { kind: 'notice'; message: string }
  | { kind: 'error'; message: string }
  | { kind: 'outcome'; label: string; reason: string };

/** Focused side panel, or none. */
export type Panel = 'none' | 'status' | 'why' | 'inspect' | 'pause-help';

export type TuiViewModel = {
  /** Completed and in-flight entries, capped at the last {@link TRANSCRIPT_CAP}. */
  readonly transcript: readonly TranscriptEntry[];
  readonly panel: Panel;
  /** Rendered text for the active panel (inspect body, status summary, ...). */
  readonly panelContent: string;
  /** A controller operation is in flight. */
  readonly busy: boolean;
  readonly lastError: string | null;
  readonly exitRequested: boolean;
  readonly columns: number;
};

export type TuiAction =
  | { type: 'controller-event'; event: ControllerUiEvent }
  | { type: 'user-input'; text: string }
  | { type: 'panel'; panel: Panel; content?: string }
  | { type: 'busy'; busy: boolean }
  | { type: 'error'; message: string | null }
  | { type: 'resize'; columns: number }
  | { type: 'exit' };

/** Transcript keeps at most the last 500 entries. */
export const TRANSCRIPT_CAP = 500;

export const OUTCOME_LABELS = ['CONTINUE', 'ADVANCE', 'NEEDS_USER', 'REVISE'] as const;

export type ViewModelHandle = {
  getState(): TuiViewModel;
  dispatch(action: TuiAction): void;
};

type Draft = {
  transcript: TranscriptEntry[];
  panel: Panel;
  panelContent: string;
  busy: boolean;
  lastError: string | null;
  exitRequested: boolean;
  columns: number;
};

const DEFAULT_COLUMNS = 80;

function initialState(): Draft {
  return {
    transcript: [],
    panel: 'none',
    panelContent: '',
    busy: false,
    lastError: null,
    exitRequested: false,
    columns: DEFAULT_COLUMNS,
  };
}

/** Reads a string field, tolerating aliases and absent values. */
function pickString(source: unknown, keys: readonly string[]): string {
  if (typeof source !== 'object' || source === null) return '';
  const record = source as Record<string, unknown>;
  for (const key of keys) {
    const value = record[key];
    if (typeof value === 'string') return value;
    if (typeof value === 'number') return String(value);
  }
  return '';
}

function pickBoolean(source: unknown, key: string): boolean | undefined {
  if (typeof source !== 'object' || source === null) return undefined;
  const value = (source as Record<string, unknown>)[key];
  return typeof value === 'boolean' ? value : undefined;
}

function pickNumber(source: unknown, key: string): number {
  if (typeof source !== 'object' || source === null) return 0;
  const value = (source as Record<string, unknown>)[key];
  return typeof value === 'number' && Number.isFinite(value) ? value : 0;
}

function capTranscript(entries: TranscriptEntry[]): TranscriptEntry[] {
  if (entries.length <= TRANSCRIPT_CAP) return entries;
  return entries.slice(entries.length - TRANSCRIPT_CAP);
}

function pushEntry(draft: Draft, entry: TranscriptEntry): void {
  draft.transcript = capTranscript([...draft.transcript, entry]);
}

/** Normalizes an outcome label to the four documented kinds. */
function outcomeLabel(value: unknown): string {
  const raw = typeof value === 'string' ? value : pickString(value, ['kind', 'label', 'outcome']);
  const normalized = raw.toUpperCase().replace(/[\s-]+/g, '_');
  return (OUTCOME_LABELS as readonly string[]).includes(normalized) ? normalized : 'NEEDS_USER';
}

function foldControllerEvent(draft: Draft, event: ControllerUiEvent): void {
  switch (event.kind) {
    case 'session': {
      const message = pickString(event, ['message', 'text', 'detail']);
      if (message) pushEntry(draft, { kind: 'notice', message });
      return;
    }
    case 'mode': {
      const mode = pickString(event, ['mode', 'to']) || 'discovery';
      const rawPrevious = (event as unknown as Record<string, unknown>)['previous'];
      const previous = typeof rawPrevious === 'string' && rawPrevious.length > 0 ? rawPrevious : null;
      const reason = pickString(event, ['reason', 'detail']);
      pushEntry(draft, { kind: 'mode', mode, previous, reason });
      return;
    }
    case 'delta': {
      const text = pickString(event, ['text', 'delta', 'content', 'message']);
      if (!text) return;
      const last = draft.transcript[draft.transcript.length - 1];
      if (last && last.kind === 'assistant') {
        const merged: TranscriptEntry = { kind: 'assistant', text: last.text + text };
        draft.transcript = [...draft.transcript.slice(0, -1), merged];
        return;
      }
      pushEntry(draft, { kind: 'assistant', text });
      return;
    }
    case 'tool': {
      const tool = pickString(event, ['tool', 'toolName', 'name']) || 'tool';
      const phase = pickString(event, ['phase', 'state']) === 'end' ? 'end' : 'start';
      if (phase === 'start') {
        pushEntry(draft, { kind: 'tool', tool, phase: 'start' });
        return;
      }
      const ok = pickBoolean(event, 'ok') ?? pickBoolean(event, 'success');
      const summary = pickString(event, ['summary', 'detail', 'message']);
      const entry: TranscriptEntry = { kind: 'tool', tool, phase: 'end' };
      if (ok !== undefined) entry.ok = ok;
      if (summary) entry.summary = summary;
      pushEntry(draft, entry);
      return;
    }
    case 'artifact': {
      pushEntry(draft, {
        kind: 'artifact',
        artifactId: pickString(event, ['artifactId', 'id']),
        artifactKind: pickString(event, ['artifactKind', 'kind']) || 'artifact',
        version: pickNumber(event, 'version') || pickNumber(event, 'currentVersion') || 1,
        status: pickString(event, ['status', 'state']) || 'draft',
      });
      return;
    }
    case 'outcome': {
      const payload = (event as unknown as Record<string, unknown>)['outcome'];
      const label = outcomeLabel(payload);
      const reason =
        pickString(payload, ['reason', 'message', 'detail']) || pickString(event, ['reason', 'message']);
      pushEntry(draft, { kind: 'outcome', label, reason });
      return;
    }
    case 'notice': {
      const message = pickString(event, ['message', 'text', 'detail']);
      if (message) pushEntry(draft, { kind: 'notice', message });
      return;
    }
    case 'error': {
      const message = pickString(event, ['message', 'error', 'detail']) || 'unknown controller error';
      draft.lastError = message;
      pushEntry(draft, { kind: 'error', message });
      return;
    }
  }
}

function reduce(draft: Draft, action: TuiAction): void {
  switch (action.type) {
    case 'controller-event':
      foldControllerEvent(draft, action.event);
      return;
    case 'user-input': {
      const text = action.text.trim();
      if (text) pushEntry(draft, { kind: 'user', text });
      return;
    }
    case 'panel': {
      draft.panel = action.panel;
      draft.panelContent = action.content ?? '';
      return;
    }
    case 'busy':
      draft.busy = action.busy;
      return;
    case 'error': {
      if (action.message === null) {
        draft.lastError = null;
        return;
      }
      if (action.message.length === 0) {
        draft.lastError = null;
        return;
      }
      draft.lastError = action.message;
      pushEntry(draft, { kind: 'error', message: action.message });
      return;
    }
    case 'resize': {
      const columns = Number.isFinite(action.columns) ? Math.max(20, Math.floor(action.columns)) : DEFAULT_COLUMNS;
      draft.columns = columns;
      return;
    }
    case 'exit':
      draft.exitRequested = true;
      return;
  }
}

/**
 * Creates an isolated view model. Dispatch mutates an internal draft; the
 * state returned by {@link ViewModelHandle.getState} is treated as read-only
 * by callers.
 */
export function createViewModel(): ViewModelHandle {
  let draft: Draft = initialState();
  return {
    getState(): TuiViewModel {
      return draft;
    },
    dispatch(action: TuiAction): void {
      const next: Draft = { ...draft, transcript: draft.transcript };
      reduce(next, action);
      draft = next;
    },
  };
}
