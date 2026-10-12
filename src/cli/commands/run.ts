/**
 * `relay run [message]` — run the workflow.
 *
 * Interactive on a TTY (delegating to the Ink TUI) and plain otherwise so
 * scripts and CI get bounded, resumable progress: one step per invocation by
 * default, or `--auto <bound>` for run-until-pause. Controller events stream
 * as plain `→ tool` lines and bounded text so redirected output still shows
 * activity. SIGINT aborts in-flight work and exits 130.
 */

import {
  NAMED_AUTONOMY_BOUNDS,
  WORKFLOW_MODES,
  parseAutonomyBound,
  type AutonomyBound,
} from '../../core/modes.js';
import type { ControllerUiEvent, StepResult, WorkflowController } from '../../runtime/controller.js';
import { CLI_PROVIDER_OVERRIDES, createCliContext, createController, hasRelayState } from '../context.js';
import type { OutputWriter } from '../output.js';

const MAX_RUN_STEPS = 60;
const MAX_STREAMED_CHARS_PER_STEP = 4000;

function isProviderName(value: string | undefined): value is 'openai' | 'anthropic' | 'fake' {
  return value !== undefined && (CLI_PROVIDER_OVERRIDES as readonly string[]).includes(value);
}

export async function runRun(options: {
  workspaceRoot: string;
  message: string | undefined;
  auto: string | undefined;
  provider: string | undefined;
  model: string | undefined;
  interactive: boolean;
  output: OutputWriter;
}): Promise<number> {
  const { workspaceRoot, message, provider, model, interactive, output } = options;

  if (provider !== undefined && !isProviderName(provider)) {
    output.error(`unknown provider "${provider}"`);
    output.error(`Valid providers: ${CLI_PROVIDER_OVERRIDES.join(', ')}`);
    return 2;
  }
  const parsedAuto = options.auto !== undefined ? parseAutonomyBound(options.auto) : undefined;
  if (parsedAuto === null) {
    output.error(`invalid autonomy bound "${options.auto}"`);
    output.error(
      `Valid bounds: ${[...NAMED_AUTONOMY_BOUNDS, ...WORKFLOW_MODES.map((mode) => `until:${mode}`)].join(', ')}`,
    );
    return 2;
  }
  const autoBound: AutonomyBound | undefined = parsedAuto;

  // Fail the usage error before touching the filesystem: a bare `relay run`
  // on an uninitialized workspace must not create `.relay/`.
  if (!hasRelayState(workspaceRoot) && (message === undefined || message.trim().length === 0)) {
    output.error('relay run requires a goal message for a new project');
    output.error('Usage: relay run "<goal>"');
    return 2;
  }

  const context = createCliContext({
    workspaceRoot,
    ...(isProviderName(provider) ? { providerOverride: provider } : {}),
    ...(model !== undefined ? { modelOverride: model } : {}),
  });
  try {
    const useTui = interactive && process.stdout.isTTY === true && process.stdin.isTTY === true;
    const guard = createInterruptGuard(output, useTui);
    const controller = await createController(context, { signal: guard.signal });
    try {
      if (useTui) {
        // The TUI owns initialization for fresh projects: its initialMessage
        // path runs initialize + first step itself, so the goal is submitted
        // exactly once.
        const { runInteractive } = await import('../../tui/app.js');
        const result = await runInteractive({
          controller,
          ...(message !== undefined && message.trim().length > 0 ? { initialMessage: message } : {}),
          signal: guard.signal,
          requestCancel: () => guard.requestCancel(),
        });
        return result.exitCode;
      }

      const fresh = controller.projectId === null;
      if (fresh) {
        if (message === undefined || message.trim().length === 0) {
          output.error('relay run requires a goal message for a new project');
          output.error('Usage: relay run "<goal>"');
          return 2;
        }
        await controller.initialize(message);
      }

      const hasMessage = message !== undefined && message.trim().length > 0;
      if (!fresh && hasMessage && message !== undefined && message.startsWith('/')) {
        // Slash commands are a single bounded interaction, not a run.
        const result = await controller.handleInput(message);
        printStepSummary(output, controller, result);
        return 0;
      }

      if (autoBound !== undefined) {
        const applied = await controller.setAutonomy(autoBound);
        output.text(`autonomy: ${applied.state.autonomy}`);
      }

      const activity = attachActivityPrinter(controller, output);
      try {
        const errorBefore = fresh ? null : controller.state.stats.lastModelError;
        let result: StepResult;
        if (!fresh && message !== undefined && message.trim().length > 0) {
          result = await controller.handleInput(message);
          if (autoBound !== undefined) {
            activity.reset();
            result = await controller.runUntilPause({ maxSteps: MAX_RUN_STEPS });
          }
        } else if (autoBound !== undefined) {
          result = await controller.runUntilPause({ maxSteps: MAX_RUN_STEPS });
        } else {
          result = await controller.runStep();
        }
        printStepSummary(output, controller, result);

        // Fail (non-zero) only when THIS run failed, so a long-resolved error
        // from an earlier session does not poison every later invocation.
        const modelError = result.state.stats.lastModelError;
        if (modelError !== null && modelError.length > 0 && modelError !== errorBefore) {
          output.error(`model error (state is resumable): ${modelError}`);
          output.error('Rerun `relay run` to retry.');
          return 1;
        }
        return 0;
      } finally {
        activity.unsubscribe();
      }
    } finally {
      guard.dispose();
    }
  } finally {
    context.close();
  }
}

/** Print the post-step summary: mode, outcome, pause state, next action. */
function printStepSummary(
  output: OutputWriter,
  controller: WorkflowController,
  result: StepResult,
): void {
  output.text(`mode: ${result.state.currentMode}`);
  const outcome = result.outcome;
  switch (outcome.kind) {
    case 'ADVANCE':
      output.text(`outcome: advance to ${outcome.to} — ${outcome.reason}`);
      break;
    case 'NEEDS_USER':
      output.text(`outcome: needs user input — ${outcome.reason}`);
      output.text(`question: ${outcome.question}`);
      for (const option of outcome.options) output.text(`  option: ${option}`);
      break;
    case 'REVISE':
      output.text(`outcome: revise ${outcome.stage} — ${outcome.reason}`);
      break;
    default:
      output.text(`outcome: continue — ${outcome.reason}`);
      break;
  }
  if (result.state.paused !== null) {
    output.text(`paused: ${result.state.paused.reason}`);
    if (result.state.paused.question !== null) {
      output.text(`  question: ${result.state.paused.question}`);
    }
  }
  const next = nextActionOf(controller);
  if (next !== null) output.text(`Next: ${next}`);
}

function nextActionOf(controller: WorkflowController): string | null {
  const view = controller.statusView() as unknown as Record<string, unknown>;
  for (const key of ['nextAction', 'next', 'suggestedAction']) {
    const value = view[key];
    if (typeof value === 'string' && value.trim().length > 0) return value;
  }
  return null;
}

/**
 * Stream controller activity as plain text: `→ tool <name>` lines plus text
 * deltas, with a per-step character cap so redirected logs stay bounded.
 */
function attachActivityPrinter(
  controller: WorkflowController,
  output: OutputWriter,
): { unsubscribe(): void; reset(): void } {
  let budget = MAX_STREAMED_CHARS_PER_STEP;
  let buffer = '';
  const emitLine = (line: string): void => {
    if (budget <= 0 || line.length === 0) return;
    const slice = line.slice(0, budget);
    budget -= slice.length;
    output.text(slice);
  };
  const pushText = (chunk: string): void => {
    buffer += chunk;
    let newline = buffer.indexOf('\n');
    while (newline !== -1) {
      emitLine(buffer.slice(0, newline));
      buffer = buffer.slice(newline + 1);
      newline = buffer.indexOf('\n');
    }
    if (buffer.length > 400) {
      emitLine(buffer);
      buffer = '';
    }
  };
  const listener = (event: ControllerUiEvent): void => {
    const record = event as unknown as Record<string, unknown>;
    // Controller events discriminate on `kind`; `type` is accepted as a
    // fallback so the printer survives small event-contract drift.
    const kind =
      typeof record['kind'] === 'string' ? record['kind'] : typeof record['type'] === 'string' ? record['type'] : '';
    if (kind === 'tool' || kind.includes('tool')) {
      const tool = record['tool'];
      const name =
        typeof tool === 'string'
          ? tool
          : typeof record['name'] === 'string'
            ? record['name']
            : 'unknown';
      const summary = typeof record['summary'] === 'string' && record['summary'].length > 0 ? ` — ${record['summary']}` : '';
      output.text(`→ tool ${name}${summary}`);
      return;
    }
    if (kind === 'delta' || kind.includes('delta') || kind.includes('text')) {
      const text = typeof record['text'] === 'string' ? record['text'] : '';
      if (text.length > 0) pushText(text);
    }
  };
  const unsubscribe = controller.subscribe(listener);
  return {
    unsubscribe(): void {
      if (buffer.trim().length > 0) emitLine(buffer.trim());
      buffer = '';
      unsubscribe();
    },
    reset(): void {
      budget = MAX_STREAMED_CHARS_PER_STEP;
    },
  };
}

/**
 * Owns Ctrl-C for the run command: aborts the controller's cancellation
 * signal and exits 130. In interactive mode the TUI receives the abort via
 * `requestCancel` and restores the terminal itself; a grace-period failsafe
 * still hard-exits if the TUI hangs.
 */
function createInterruptGuard(
  output: OutputWriter,
  interactive: boolean,
): { signal: AbortSignal; requestCancel(): void; dispose(): void } {
  const abort = new AbortController();
  let failsafe: NodeJS.Timeout | undefined;
  const onSigint = (): void => {
    if (abort.signal.aborted) return;
    abort.abort();
    if (!interactive) {
      output.error('\nrelay: interrupted');
      process.exit(130);
    }
    failsafe = setTimeout(() => {
      output.error('\nrelay: interrupted');
      process.exit(130);
    }, 1500);
  };
  process.on('SIGINT', onSigint);
  return {
    signal: abort.signal,
    requestCancel(): void {
      // Cooperative cancel: abort in-flight work without force-exiting so the
      // TUI can unmount and restore the terminal first.
      abort.abort();
    },
    dispose(): void {
      if (failsafe !== undefined) clearTimeout(failsafe);
      process.removeListener('SIGINT', onSigint);
    },
  };
}
