/**
 * Deterministic tool permission policy.
 *
 * Every tool call passes through `evaluateToolPermission` before execution.
 * Decisions derive only from the workflow mode contract, the autonomy bound,
 * the pause state, and — for execute tools — a conservative denylist of
 * destructive or delivery-class commands. Nothing here consults a model.
 */

import { MODE_CONTRACTS, type AutonomyBound, type WorkflowMode } from '../core/modes.js';
import type { ProjectState } from '../core/project-state.js';
import type { PermissionDecision, PermissionGate, RegisteredTool } from '../tools/registry.js';

/**
 * Commands Relay never runs on the user's behalf. Matched against the joined
 * `command` + `args` of an execute tool call; matching is conservative.
 */
export const DENIED_COMMAND_PATTERNS: readonly { readonly pattern: RegExp; readonly reason: string }[] = [
  {
    pattern: /\bgit\s+push\b/,
    reason: 'git push changes the remote repository; delivery actions require explicit user authorization',
  },
  {
    pattern: /\bgit\s+reset\s+--hard\b/,
    reason: 'git reset --hard discards user changes; Relay never destroys user work',
  },
  {
    pattern: /\bgit\s+clean\b/,
    reason: 'git clean deletes untracked user files; Relay never destroys user work',
  },
  {
    pattern: /\bgit\s+checkout\s+(--|\.(?:\s|$))/,
    reason: 'git checkout over paths discards user changes; Relay never restores over user work',
  },
  {
    pattern: /\bgit\s+stash\b/,
    reason: 'git stash hides user changes; Relay never moves user work aside automatically',
  },
  {
    pattern: /\bgit\s+commit\b/,
    reason: 'git commit is a delivery action; Relay never commits — the user commits explicitly',
  },
  {
    pattern: /\bnpm\s+publish\b/,
    reason: 'npm publish releases a package; delivery actions require explicit user authorization',
  },
  {
    pattern: /\brm\b(?=[^\n;|&]*\s-[A-Za-z]*r)(?=[^\n;|&]*\s-[A-Za-z]*f)/,
    reason:
      'rm with recursive and force flags deletes files irrecoverably; ' +
      'destructive operations require explicit user authorization',
  },
  {
    pattern: /\bmkfs\b/,
    reason: 'mkfs formats a filesystem; destructive operations require explicit user authorization',
  },
  {
    pattern: /\bdd\b[^\n;|]*\bof=/,
    reason: 'dd with of= overwrites devices or files; destructive operations require explicit user authorization',
  },
  {
    pattern: /\b(curl|wget)\b[^\n;|]*\|\s*(sudo\s+)?(ba|z|da)?sh\b/,
    reason: 'piping a download into a shell runs unreviewed code from the network',
  },
  {
    pattern: /\bchmod\s+-R\s+777\b/,
    reason: 'chmod -R 777 weakens permissions for the whole tree',
  },
];

export type PermissionContext = {
  readonly mode: WorkflowMode;
  readonly autonomy: AutonomyBound;
  readonly state: ProjectState;
};

/**
 * Best-effort extraction of the command an execute tool will run. Returns
 * null for argument shapes without a string `command` field — those are
 * validated against the tool's own schema upstream.
 */
function extractCommandFragment(args: unknown): string | null {
  if (typeof args !== 'object' || args === null) return null;
  const record = args as Readonly<Record<string, unknown>>;
  const command = record['command'];
  if (typeof command !== 'string' || command.length === 0) return null;
  const rest = record['args'];
  if (!Array.isArray(rest)) return command;
  return [command, ...rest.map((part) => String(part))].join(' ');
}

export function evaluateToolPermission(
  tool: RegisteredTool,
  args: unknown,
  context: PermissionContext,
): PermissionDecision {
  const contract = MODE_CONTRACTS[context.mode];

  if (context.state.paused !== null) {
    return {
      decision: 'deny',
      reason: `workflow is paused (${context.state.paused.reason}); resume with /resume before running any tool`,
      needsUser: true,
    };
  }

  if (!contract.permittedToolCapabilities.includes(tool.capability)) {
    return {
      decision: 'deny',
      reason:
        `mode ${context.mode} permits only ${contract.permittedToolCapabilities.join('/')} tools; ` +
        `"${tool.name}" is a ${tool.capability} tool`,
      needsUser: false,
    };
  }

  if (tool.capability === 'write' && !contract.canModifyUserFiles) {
    return {
      decision: 'deny',
      reason: `mode ${context.mode} may not modify user files; "${tool.name}" writes to the workspace`,
      needsUser: false,
    };
  }

  if (context.autonomy === 'off' && tool.capability !== 'read') {
    return {
      decision: 'deny',
      reason: `autonomy is off; the user must run ${tool.capability} tools like "${tool.name}" manually`,
      needsUser: true,
    };
  }

  if (tool.capability === 'execute') {
    const fragment = extractCommandFragment(args);
    if (fragment !== null) {
      for (const denied of DENIED_COMMAND_PATTERNS) {
        const match = fragment.match(denied.pattern);
        if (match !== null) {
          return {
            decision: 'deny',
            reason: `denied command fragment "${match[0]}" in "${fragment}": ${denied.reason}`,
            needsUser: true,
          };
        }
      }
    }
  }

  return { decision: 'allow' };
}

/** Adapt `evaluateToolPermission` into the registry's `PermissionGate` shape. */
export function createPermissionGate(context: PermissionContext): PermissionGate {
  return (tool, args) => evaluateToolPermission(tool, args, context);
}
