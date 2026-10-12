/**
 * Interactive terminal application.
 *
 * `runInteractive` mounts the Ink app over a workflow controller and resolves
 * with a process exit code once the app unmounts. The React tree only renders
 * view-model state and dispatches actions: all controller orchestration lives
 * in `src/runtime`, and all folding lives in `src/tui/view-model.ts`.
 *
 * Terminal safety: the app is rendered with `exitOnCtrlC: false` so ctrl-c is
 * a first-class action (cancel when busy, exit when idle, force-exit 130 on a
 * second press while busy). Every exit path funnels through Ink's unmount so
 * raw mode is always restored.
 */

import { useCallback, useEffect, useRef, type ReactElement } from 'react';
import { Box, render, Text, useApp, useInput, useStdout, type Key } from 'ink';
import { readControllerState, type TuiController } from './view-model.js';
import { useControllerFeed } from './hooks/use-controller-feed.js';
import { Header } from './components/header.js';
import { Transcript } from './components/transcript.js';
import { InputLine } from './components/input-line.js';
import { PauseOverlay } from './components/pause-overlay.js';
import { PanelFrame, StatusPanel } from './components/status-panel.js';
import { WhyPanel } from './components/why-panel.js';
import { InspectPanel } from './components/inspect-panel.js';

export type { TuiController } from './view-model.js';

export type InteractiveOptions = {
  readonly controller: TuiController;
  /** Goal to initialize a fresh project with, or a first message for an existing one. */
  readonly initialMessage?: string | undefined;
  /** Aborted by the CLI (signals, argument errors) to tear the interface down. */
  readonly signal: AbortSignal;
  readonly stdin?: NodeJS.ReadableStream | undefined;
  readonly stdout?: NodeJS.WritableStream | undefined;
  /** Wired by the CLI to its AbortController; cancels the in-flight controller op. */
  readonly requestCancel?: (() => void) | undefined;
};

export type AppProps = {
  readonly controller: TuiController;
  readonly initialMessage?: string | undefined;
  readonly requestCancel?: (() => void) | undefined;
  /** Receives the exit code right before the Ink app unmounts. */
  readonly onExit: (code: number) => void;
  /** Overrides terminal width (used by narrow-width render tests). */
  readonly fixedColumns?: number | undefined;
};

const DEFAULT_COLUMNS = 80;

const HELP_TEXT = [
  'commands:',
  '  /status                 where things stand',
  '  /why                    last routing or transition decision',
  '  /inspect <artifact>     read an artifact markdown',
  '  /mode <mode>            override the active workflow mode',
  '  /auto <bound>           off | next-checkpoint | until:<mode> | workflow',
  '  /pause [reason]         stop automatic advancement',
  '  /resume                 continue',
  '  /approve [artifact]     approve an artifact',
  '  /revise <kind> <why>    send an approved artifact back',
  '  /help                   this list',
  '  /quit                   leave the session',
  'anything else is sent to relay as conversational input.',
].join('\n');

export function errorMessage(error: unknown): string {
  if (error instanceof Error) {
    return error.message.length > 0 ? error.message : error.name;
  }
  if (typeof error === 'string') return error.length > 0 ? error : 'unknown error';
  if (error === null || error === undefined) return 'unknown error';
  try {
    const serialized = JSON.stringify(error);
    return typeof serialized === 'string' ? serialized : 'unknown error';
  } catch {
    return 'unknown error';
  }
}

function normalizeKey(key: string): string {
  return key.toLowerCase().replace(/[^a-z0-9]/g, '');
}

const STATUS_KEY_ORDER = [
  'goal',
  'mode',
  'stage',
  'progress',
  'activework',
  'blockers',
  'artifacts',
  'changedfiles',
  'nextaction',
  'autonomy',
];

const WHY_KEY_ORDER = [
  'decision',
  'question',
  'recommendation',
  'reason',
  'evidence',
  'alternatives',
  'mode',
  'autonomy',
];

const INSPECT_KEY_ORDER = [
  'title',
  'kind',
  'version',
  'status',
  'path',
  'updatedAt',
  'markdown',
  'body',
];

function formatScalar(value: unknown): string | null {
  if (value === null || value === undefined) return null;
  if (typeof value === 'string') return value.length > 0 ? value : null;
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  return null;
}

function humanizeKey(key: string): string {
  const spaced = key.replace(/([a-z0-9])([A-Z])/g, '$1 $2').replace(/[_-]+/g, ' ');
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

function renderRecord(record: Record<string, unknown>, indent: string, depth: number): string[] {
  const lines: string[] = [];
  for (const [key, value] of Object.entries(record)) {
    const label = humanizeKey(key);
    if (Array.isArray(value)) {
      if (value.length === 0) {
        lines.push(`${indent}${label}: none`);
        continue;
      }
      const scalars = value.map((item) => formatScalar(item));
      if (scalars.every((item) => item !== null)) {
        lines.push(`${indent}${label}: ${scalars.join(', ')}`);
        continue;
      }
      lines.push(`${indent}${label}:`);
      for (const item of value) {
        if (typeof item === 'object' && item !== null && depth < 2) {
          const nested = renderRecord(item as Record<string, unknown>, `${indent}  `, depth + 1);
          lines.push(`${indent}  -`);
          lines.push(...nested.map((line) => `${line}`));
        } else {
          const scalar = formatScalar(item);
          lines.push(`${indent}  - ${scalar ?? errorMessage(item)}`);
        }
      }
      continue;
    }
    if (typeof value === 'object' && value !== null) {
      if (depth < 2) {
        const nested = renderRecord(value as Record<string, unknown>, `${indent}  `, depth + 1);
        if (nested.length > 0) {
          lines.push(`${indent}${label}:`);
          lines.push(...nested);
        }
        continue;
      }
      const serialized = formatScalar(JSON.stringify(value));
      if (serialized !== null) lines.push(`${indent}${label}: ${serialized}`);
      continue;
    }
    const scalar = formatScalar(value);
    if (scalar !== null) lines.push(`${indent}${label}: ${scalar}`);
  }
  return lines;
}

function orderEntries(
  record: Record<string, unknown>,
  preferredKeys: readonly string[],
): [string, unknown][] {
  const entries = Object.entries(record);
  const ranked: [string, unknown][] = [];
  const remaining = new Map(entries);
  for (const preferred of preferredKeys) {
    for (const [key, value] of entries) {
      if (normalizeKey(key) === preferred) {
        ranked.push([key, value]);
        remaining.delete(key);
      }
    }
  }
  return [...ranked, ...remaining];
}

/** Renders a controller view (`statusView()` / `whyView()`) defensively to text. */
export function renderViewText(view: unknown, preferredKeys: readonly string[]): string {
  if (view === null || view === undefined) return '(no data)';
  if (typeof view === 'string') return view.length > 0 ? view : '(no data)';
  if (typeof view !== 'object') return String(view);
  const record = view as Record<string, unknown>;
  const ordered = orderEntries(record, preferredKeys);
  const lines = renderRecord(Object.fromEntries(ordered), '  ', 0);
  return lines.length > 0 ? lines.join('\n') : '(no data)';
}

/** Renders an inspect result: markdown body when available, else its fields. */
export function renderInspectBody(result: unknown): string {
  if (result === null || result === undefined) return '(nothing to inspect)';
  if (typeof result === 'string') return result.length > 0 ? result : '(nothing to inspect)';
  if (typeof result !== 'object') return String(result);
  const record = result as Record<string, unknown>;
  for (const key of ['markdown', 'body', 'content', 'text']) {
    const value = record[key];
    if (typeof value === 'string' && value.length > 0) return value;
  }
  return renderViewText(result, INSPECT_KEY_ORDER);
}

function isUninitialized(controller: TuiController): boolean {
  const projectId = controller.projectId;
  if (projectId !== undefined) {
    return projectId === null || projectId.length === 0;
  }
  try {
    return controller.state.projectId.length === 0;
  } catch {
    return true;
  }
}

/** The interactive Relay app. Rendered through {@link runInteractive}. */
export function App({
  controller,
  initialMessage,
  requestCancel,
  onExit,
  fixedColumns,
}: AppProps): ReactElement {
  const { vm, dispatch, snapshot, refresh } = useControllerFeed(controller);
  const { exit } = useApp();
  const { stdout } = useStdout();
  // Degenerate terminals report columns as 0; treat that as unknown width.
  const terminalColumns = stdout.columns;
  const columns =
    fixedColumns
    ?? (terminalColumns !== undefined && terminalColumns > 0 ? terminalColumns : vm.columns)
    ?? DEFAULT_COLUMNS;

  const busyRef = useRef(vm.busy);
  busyRef.current = vm.busy;

  const cancelArmedRef = useRef(false);
  const outcomeSeenRef = useRef<string | null>(null);

  const requestExit = useCallback(
    (code: number) => {
      dispatch({ type: 'exit' });
      onExit(code);
      exit();
    },
    [dispatch, onExit, exit],
  );

  useEffect(() => {
    dispatch({ type: 'resize', columns });
  }, [columns, dispatch]);

  useEffect(() => {
    const listener = (): void => {
      const next = stdout.columns;
      if (next !== undefined && next > 0) {
        dispatch({ type: 'resize', columns: next });
      }
    };
    stdout.on('resize', listener);
    return () => {
      stdout.off('resize', listener);
    };
  }, [stdout, dispatch]);

  const submitRef = useRef<(value: string) => void>(() => {});
  submitRef.current = (value: string): void => {
    const text = value.trim();
    if (text.length === 0) return;
    if (busyRef.current) return;

    // Any submit acknowledges a pending NEEDS_USER outcome.
    const current = readControllerState(controller);
    if (current.lastOutcome !== null && current.lastOutcome.outcome === 'NEEDS_USER') {
      outcomeSeenRef.current = current.lastOutcome.at;
    }

    dispatch({ type: 'user-input', text });

    if (text === '/quit' || text === '/exit') {
      requestExit(0);
      return;
    }
    if (text === '/help' || text === '/?') {
      dispatch({ type: 'panel', panel: 'pause-help', content: HELP_TEXT });
      return;
    }
    if (text === '/status') {
      try {
        const content = renderViewText(controller.statusView(), STATUS_KEY_ORDER);
        dispatch({ type: 'panel', panel: 'status', content });
      } catch (error) {
        dispatch({ type: 'error', message: errorMessage(error) });
      }
      refresh();
      return;
    }
    if (text === '/why') {
      try {
        const content = renderViewText(controller.whyView(), WHY_KEY_ORDER);
        dispatch({ type: 'panel', panel: 'why', content });
      } catch (error) {
        dispatch({ type: 'error', message: errorMessage(error) });
      }
      refresh();
      return;
    }
    if (text.startsWith('/inspect')) {
      const target = text.slice('/inspect'.length).trim();
      if (target.length === 0) {
        dispatch({ type: 'error', message: 'usage: /inspect <artifact-id-or-kind>' });
        return;
      }
      dispatch({ type: 'busy', busy: true });
      void (async () => {
        try {
          const body = await controller.inspect(target);
          dispatch({ type: 'panel', panel: 'inspect', content: renderInspectBody(body) });
        } catch (error) {
          dispatch({ type: 'error', message: errorMessage(error) });
        } finally {
          dispatch({ type: 'busy', busy: false });
          refresh();
        }
      })();
      return;
    }

    dispatch({ type: 'busy', busy: true });
    void (async () => {
      try {
        await controller.handleInput(text);
      } catch (error) {
        dispatch({ type: 'error', message: errorMessage(error) });
      } finally {
        dispatch({ type: 'busy', busy: false });
        refresh();
      }
    })();
  };

  const ctrlCRef = useRef<(input: string, key: Key) => void>(() => {});
  ctrlCRef.current = (input: string, key: Key): void => {
    if (!key.ctrl || input !== 'c') return;
    if (!busyRef.current) {
      requestExit(0);
      return;
    }
    if (requestCancel !== undefined && !cancelArmedRef.current) {
      cancelArmedRef.current = true;
      dispatch({
        type: 'controller-event',
        event: { kind: 'notice', message: 'cancel requested — ctrl-c again to force exit' },
      });
      requestCancel();
      return;
    }
    requestExit(130);
  };
  const stableCtrlC = useCallback((input: string, key: Key): void => {
    ctrlCRef.current(input, key);
  }, []);
  useInput(stableCtrlC);

  // First-run affordance: initialize a fresh project, or send the message.
  const bootstrappedRef = useRef(false);
  useEffect(() => {
    if (bootstrappedRef.current) return;
    bootstrappedRef.current = true;
    const message = initialMessage?.trim();
    if (message === undefined || message.length === 0) return;
    const fresh = isUninitialized(controller);
    dispatch({ type: 'user-input', text: message });
    dispatch({ type: 'busy', busy: true });
    void (async () => {
      try {
        if (fresh) {
          await controller.initialize(message);
          await controller.runStep();
        } else {
          await controller.handleInput(message);
        }
      } catch (error) {
        dispatch({ type: 'error', message: errorMessage(error) });
      } finally {
        cancelArmedRef.current = false;
        dispatch({ type: 'busy', busy: false });
        refresh();
      }
    })();
  }, [controller, initialMessage, dispatch, refresh]);

  const lastOutcome = snapshot.lastOutcome;
  const needsUser = lastOutcome !== null && lastOutcome.outcome === 'NEEDS_USER' && lastOutcome.at !== outcomeSeenRef.current;
  const paused = snapshot.paused;
  const pauseVisible = paused !== null || needsUser;

  let panel: ReactElement | null = null;
  if (vm.panel === 'status') {
    panel = <StatusPanel content={vm.panelContent} columns={columns} />;
  } else if (vm.panel === 'why') {
    panel = <WhyPanel content={vm.panelContent} columns={columns} />;
  } else if (vm.panel === 'inspect') {
    panel = <InspectPanel content={vm.panelContent} columns={columns} />;
  } else if (vm.panel === 'pause-help') {
    panel = <PanelFrame title="help" content={vm.panelContent} columns={columns} />;
  }

  return (
    <Box flexDirection="column">
      <Header snapshot={snapshot} columns={columns} uninitialized={isUninitialized(controller)} />
      <Transcript entries={vm.transcript} />
      {pauseVisible ? (
        <PauseOverlay
          reason={paused !== null ? paused.reason : lastOutcome?.reason ?? ''}
          question={paused !== null ? paused.question : null}
          options={paused !== null ? [...paused.options] : []}
          decision={paused === null || paused.by === 'policy'}
        />
      ) : null}
      {panel}
      <InputLine busy={vm.busy} onSubmit={(value) => submitRef.current(value)} />
      <Text dimColor>{'/help for commands · enter to send · ctrl-c exits'}</Text>
    </Box>
  );
}

/**
 * Mounts the interactive app and resolves with the exit code once it unmounts.
 * Restores the terminal on every path (Ink unmount is idempotent) and exits
 * 130 when the caller's abort signal fires.
 */
export async function runInteractive(options: InteractiveOptions): Promise<{ exitCode: number }> {
  if (options.signal.aborted) {
    return { exitCode: 130 };
  }
  const stdout = options.stdout ?? process.stdout;
  const stdin = options.stdin ?? process.stdin;
  let exitCode = 0;

  const instance = render(
    <App
      controller={options.controller}
      initialMessage={options.initialMessage}
      requestCancel={options.requestCancel}
      onExit={(code) => {
        exitCode = code;
      }}
    />,
    {
      stdin: stdin as NodeJS.ReadStream,
      stdout: stdout as NodeJS.WriteStream,
      exitOnCtrlC: false,
      patchConsole: false,
    },
  );

  const onAbort = (): void => {
    exitCode = 130;
    instance.unmount();
  };
  options.signal.addEventListener('abort', onAbort, { once: true });

  try {
    await instance.waitUntilExit();
  } finally {
    instance.unmount();
    options.signal.removeEventListener('abort', onAbort);
  }

  if ((stdout as { isTTY?: boolean }).isTTY === true && exitCode !== 130) {
    stdout.write('\nrelay: session ended — run relay status anytime\n');
  }
  return { exitCode };
}
