#!/usr/bin/env node
/**
 * Relay executable entry point.
 *
 * Deliberately tiny: everything substantive lives in `src/cli/main.ts`. All
 * output is written through synchronous writers (or drained by the TUI)
 * before the process exits, so piping `relay ... > file` never truncates.
 */

import { runCli } from './cli/main.js';

function describeInternalError(reason: unknown): string {
  if (reason instanceof Error) return reason.message;
  if (typeof reason === 'string') return reason;
  return String(reason);
}

process.on('unhandledRejection', (reason) => {
  process.stderr.write(`relay: internal error: ${describeInternalError(reason)}\n`);
  process.exit(1);
});

process.on('uncaughtException', (error) => {
  process.stderr.write(`relay: internal error: ${describeInternalError(error)}\n`);
  process.exit(1);
});

const exitCode = await runCli(process.argv.slice(2));
process.exit(exitCode);
