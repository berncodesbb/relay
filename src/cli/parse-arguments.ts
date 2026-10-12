/**
 * Command-line argument parsing.
 *
 * Pure and side-effect free so it can be tested and reused by both the
 * interactive and non-interactive entry paths. Commands map one-to-one onto
 * the commands documented in README.md.
 */

import { isAbsolute, resolve } from 'node:path';

export const USAGE = `relay — carry product intent from discovery through delivery

Usage:
  relay init [--goal <text>]                 Create local Relay state (.relay/)
  relay run [message] [options]              Run the workflow (interactive by default)
  relay status [--json]                      Show goal, mode, progress, artifacts, next action
  relay inspect <artifact>                   Print an artifact's Markdown
  relay mode <mode>                          Override the active workflow mode
  relay auto [off|next-checkpoint|until:<mode>|workflow]
                                             Change the autonomy bound
  relay pause [<reason>]                     Pause automatic advancement
  relay why                                  Explain the last routing/transition decision
  relay config [show|path|get <key>]         Inspect effective configuration

Options:
  --workspace <path>     Operate on this directory (default: current directory)
  --no-interactive       Force plain, non-interactive output (scripts and CI)
  --provider <name>      Force a provider for this invocation (openai|anthropic|fake)
  --model <name>         Override the routed model for this invocation
  --auto <bound>         Set the autonomy bound for a run
  --json                 Machine-readable output where supported
  --no-color             Disable ANSI colors
  -h, --help             Show this help
  -V, --version          Show version

Workflow modes:
  discovery, specification, technical-design, planning, implementation,
  verification, delivery

Slash commands (inside relay run):
  /status /why /inspect <artifact> /mode <mode> /auto <bound> /pause <reason>
  /resume /approve [artifact] /revise <kind> <reason> /help
`;

export type ParsedArguments =
  | { readonly command: 'help' }
  | { readonly command: 'version' }
  | { readonly command: 'init'; readonly goal: string | undefined }
  | {
    readonly command: 'run';
    readonly message: string | undefined;
    readonly auto: string | undefined;
    readonly provider: string | undefined;
    readonly model: string | undefined;
    readonly interactive: boolean;
    readonly json: boolean;
  }
  | { readonly command: 'status'; readonly json: boolean }
  | { readonly command: 'inspect'; readonly target: string; readonly json: boolean }
  | { readonly command: 'mode'; readonly mode: string }
  | { readonly command: 'auto'; readonly bound: string | undefined }
  | { readonly command: 'pause'; readonly reason: string | undefined }
  | { readonly command: 'why'; readonly json: boolean }
  | {
    readonly command: 'config';
    readonly action: 'show' | 'path' | 'get';
    readonly key: string | undefined;
    readonly json: boolean;
  };

export type ParseResult =
  | { readonly ok: true; readonly value: ParsedArguments; readonly workspace: string }
  | { readonly ok: false; readonly error: string; readonly usage: boolean };

const COMMANDS = [
  'init',
  'run',
  'status',
  'inspect',
  'mode',
  'auto',
  'pause',
  'why',
  'config',
] as const;

type CommonOptions = {
  workspace: string;
  provider: string | undefined;
  model: string | undefined;
  interactive: boolean;
  json: boolean;
};

function parseCommon(flags: Map<string, string | boolean>): CommonOptions {
  const workspaceFlag = flags.get('workspace');
  const workspace =
    typeof workspaceFlag === 'string' && workspaceFlag.length > 0
      ? isAbsolute(workspaceFlag)
        ? workspaceFlag
        : resolve(process.cwd(), workspaceFlag)
      : process.cwd();
  const provider = typeof flags.get('provider') === 'string' ? (flags.get('provider') as string) : undefined;
  const model = typeof flags.get('model') === 'string' ? (flags.get('model') as string) : undefined;
  const json = flags.get('json') === true;
  const interactive = flags.get('no-interactive') !== true;
  return { workspace, provider, model, interactive, json };
}

/**
 * Splits argv into the leading command, positional operands, and flags.
 * Flags may be `--name value`, `--name=value`, or boolean `--name`.
 */
function tokenize(argv: readonly string[]): {
  command: string | undefined;
  positionals: string[];
  flags: Map<string, string | boolean>;
  errors: string[];
} {
  const flags = new Map<string, string | boolean>();
  const positionals: string[] = [];
  const errors: string[] = [];
  let command: string | undefined;
  let i = 0;
  const valueFlags = new Set(['workspace', 'provider', 'model', 'auto', 'goal']);

  while (i < argv.length) {
    const token = argv[i]!;
    if (token === '--help' || token === '-h') {
      return { command: 'help', positionals, flags, errors };
    }
    if (token === '--version' || token === '-V') {
      return { command: 'version', positionals, flags, errors };
    }
    if (token.startsWith('--')) {
      const eq = token.indexOf('=');
      const name = eq === -1 ? token.slice(2) : token.slice(2, eq);
      let value: string | boolean = true;
      if (eq !== -1) {
        value = token.slice(eq + 1);
      } else if (valueFlags.has(name) && i + 1 < argv.length) {
        value = argv[i + 1]!;
        i += 1;
      }
      flags.set(name, value);
    } else if (token.startsWith('-') && token.length > 1) {
      errors.push(`unknown option "${token}" (only long options are supported)`);
    } else if (command === undefined) {
      command = token;
    } else {
      positionals.push(token);
    }
    i += 1;
  }
  return { command, positionals, flags, errors };
}

export function parseArguments(argv: readonly string[]): ParseResult {
  const { command, positionals, flags, errors } = tokenize(argv);
  if (errors.length > 0) {
    return { ok: false, error: errors.join('; '), usage: true };
  }
  if (command === 'help') return { ok: true, value: { command: 'help' }, workspace: process.cwd() };
  if (command === 'version') return { ok: true, value: { command: 'version' }, workspace: process.cwd() };
  if (command === undefined) {
    return {
      ok: false,
      error: 'no command given',
      usage: true,
    };
  }
  if (!(COMMANDS as readonly string[]).includes(command)) {
    return {
      ok: false,
      error: `unknown command "${command}"`,
      usage: true,
    };
  }

  const common = parseCommon(flags);

  switch (command as (typeof COMMANDS)[number]) {
    case 'init': {
      const goal = typeof flags.get('goal') === 'string' ? (flags.get('goal') as string) : positionals.join(' ') || undefined;
      return { ok: true, value: { command: 'init', goal }, workspace: common.workspace };
    }
    case 'run': {
      const autoFlag = flags.get('auto');
      const message = positionals.join(' ') || undefined;
      return {
        ok: true,
        value: {
          command: 'run',
          message,
          auto: typeof autoFlag === 'string' ? autoFlag : undefined,
          provider: common.provider,
          model: common.model,
          interactive: common.interactive,
          json: common.json,
        },
        workspace: common.workspace,
      };
    }
    case 'status':
      return { ok: true, value: { command: 'status', json: common.json }, workspace: common.workspace };
    case 'inspect': {
      const target = positionals[0];
      if (!target) {
        return { ok: false, error: 'inspect requires an artifact id or path', usage: false };
      }
      if (positionals.length > 1) {
        return { ok: false, error: 'inspect takes exactly one artifact id or path', usage: false };
      }
      return {
        ok: true,
        value: { command: 'inspect', target, json: common.json },
        workspace: common.workspace,
      };
    }
    case 'mode': {
      const mode = positionals[0];
      if (!mode) {
        return { ok: false, error: 'mode requires a workflow mode', usage: false };
      }
      return { ok: true, value: { command: 'mode', mode }, workspace: common.workspace };
    }
    case 'auto': {
      const bound = positionals[0] ?? (typeof flags.get('auto') === 'string' ? (flags.get('auto') as string) : undefined);
      return { ok: true, value: { command: 'auto', bound }, workspace: common.workspace };
    }
    case 'pause': {
      const reason = positionals.join(' ') || undefined;
      return { ok: true, value: { command: 'pause', reason }, workspace: common.workspace };
    }
    case 'why':
      return { ok: true, value: { command: 'why', json: common.json }, workspace: common.workspace };
    case 'config': {
      const action = positionals[0] ?? 'show';
      if (action !== 'show' && action !== 'path' && action !== 'get') {
        return {
          ok: false,
          error: `config action must be show, path, or get (got "${action}")`,
          usage: false,
        };
      }
      const key = positionals[1];
      if (action === 'get' && !key) {
        return { ok: false, error: 'config get requires a key', usage: false };
      }
      return {
        ok: true,
        value: { command: 'config', action, key, json: common.json },
        workspace: common.workspace,
      };
    }
  }
}
