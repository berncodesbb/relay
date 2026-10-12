/**
 * Non-interactive CLI orchestration.
 *
 * `runCli` parses argv, applies global flags, dispatches to a command, and
 * maps any thrown error onto a printable message plus exit code (0 success,
 * 1 runtime failure, 2 usage/configuration error). Help and version output
 * bypass the JSON-suppressing writer; everything else goes through an
 * {@link OutputWriter}.
 */

import { readFileSync, writeSync } from 'node:fs';
import { USAGE, parseArguments, type ParsedArguments } from './parse-arguments.js';
import { createOutputWriter, type OutputWriter } from './output.js';
import { describeCliError } from './errors.js';
import { createCliContext, createController, hasRelayState } from './context.js';
import type { WorkflowController } from '../runtime/controller.js';
import { runInit } from './commands/init.js';
import { runRun } from './commands/run.js';
import { runStatus } from './commands/status.js';
import { runInspect } from './commands/inspect.js';
import { runMode } from './commands/mode.js';
import { runAuto } from './commands/auto.js';
import { runPause } from './commands/pause.js';
import { runWhy } from './commands/why.js';
import { runConfig } from './commands/config.js';

export async function runCli(argv: readonly string[]): Promise<number> {
  if (argv.includes('--no-color')) process.env.NO_COLOR = '1';
  const parsed = parseArguments(argv);
  if (!parsed.ok) {
    const err = createOutputWriter({ json: false });
    err.error(`relay: ${parsed.error}`);
    if (parsed.usage) err.error('Run relay --help for usage.');
    return 2;
  }
  const output = createOutputWriter({ json: commandWantsJson(parsed.value) });
  try {
    return await dispatch(parsed.value, parsed.workspace, output);
  } catch (error) {
    const described = describeCliError(error);
    output.error(described.message);
    if (described.hint !== undefined) output.error(described.hint);
    return described.code;
  }
}

async function dispatch(
  args: ParsedArguments,
  workspace: string,
  output: OutputWriter,
): Promise<number> {
  switch (args.command) {
    case 'help':
      writeDirect(USAGE.trimEnd());
      return 0;
    case 'version':
      writeDirect(readVersion());
      return 0;
    case 'init':
      return runInit({ workspaceRoot: workspace, goal: args.goal, output });
    case 'config':
      return runConfig({ workspaceRoot: workspace, action: args.action, key: args.key, output });
    case 'run':
      return runRun({
        workspaceRoot: workspace,
        message: args.message,
        auto: args.auto,
        provider: args.provider,
        model: args.model,
        interactive: args.interactive,
        output,
      });
    case 'status':
    case 'inspect':
    case 'mode':
    case 'auto':
    case 'pause':
    case 'why':
      return dispatchStateful(args, workspace, output);
  }
}

/** Commands that need an initialized project and a live controller. */
async function dispatchStateful(
  args: ParsedArguments,
  workspace: string,
  output: OutputWriter,
): Promise<number> {
  if (!hasRelayState(workspace)) {
    output.error(`No Relay project in ${workspace}. Run relay init.`);
    return 1;
  }
  const context = createCliContext({ workspaceRoot: workspace });
  let controller: WorkflowController;
  try {
    controller = await createController(context);
  } catch (error) {
    context.close();
    throw error;
  }
  try {
    switch (args.command) {
      case 'status':
        // Every command is awaited INSIDE this try so the finally below only
        // closes the database after the command's work has settled.
        return await runStatus({ controller, output, json: args.json });
      case 'inspect':
        return await runInspect({ controller, output, target: args.target, json: args.json });
      case 'mode':
        return await runMode({ controller, output, mode: args.mode });
      case 'auto':
        return await runAuto({ controller, output, bound: args.bound });
      case 'pause':
        return await runPause({ controller, output, reason: args.reason });
      case 'why':
        return await runWhy({ controller, output, json: args.json });
      default:
        throw new Error(`internal error: unhandled command "${args.command}"`);
    }
  } finally {
    context.close();
  }
}

function commandWantsJson(args: ParsedArguments): boolean {
  return 'json' in args && (args as { json?: boolean }).json === true;
}

/** Version from the package.json that ships alongside this module. */
function readVersion(): string {
  try {
    const raw = readFileSync(new URL('../../package.json', import.meta.url), 'utf8');
    const parsed = JSON.parse(raw) as { version?: unknown };
    return typeof parsed.version === 'string' ? parsed.version : '0.0.0';
  } catch {
    return '0.0.0';
  }
}

/** Help/version output goes straight to stdout regardless of --json. */
function writeDirect(text: string): void {
  const payload = `${text}\n`;
  try {
    writeSync(process.stdout.fd, payload);
  } catch {
    process.stdout.write(payload);
  }
}
