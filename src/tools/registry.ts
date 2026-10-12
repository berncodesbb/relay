/**
 * Tool registry contracts.
 *
 * Tools are schema-validated, capability-classified, time-bounded, and
 * cancelable. The registry is the only surface the agent loop uses; it never
 * spawns processes or touches the filesystem directly.
 */

import type { ZodType } from 'zod';
import type { ToolCapability } from '../core/modes.js';
import type { JsonSchemaObject, ToolSpec } from '../providers/provider.js';

export type { ToolCapability };

export type ToolOutcome = 'ok' | 'error';

export type ToolResult = {
  readonly ok: boolean;
  readonly outcome: ToolOutcome;
  /** Human/model-readable output, already redacted and truncated by the tool. */
  readonly output: string;
  /** Structured payload for callers that want more than text. */
  readonly data?: unknown;
  /** Workspace-relative paths the tool changed (write tools only). */
  readonly affectedFiles: readonly string[];
};

export type ToolContext = {
  /** Resolved absolute workspace root; tools must not escape it. */
  readonly workspaceRoot: string;
  readonly signal: AbortSignal;
  readonly log: (message: string) => void;
};

export type ToolDefinition<A extends ZodType = ZodType> = {
  readonly name: string;
  readonly title: string;
  readonly description: string;
  readonly capability: ToolCapability;
  readonly argsSchema: A;
  readonly timeoutMs: number;
  /** Hard cap on returned output length in characters. */
  readonly maxOutputChars: number;
  execute(args: ToolArgs<A>, context: ToolContext): Promise<ToolResult>;
};

export type ToolArgs<A extends ZodType> = A extends ZodType<infer T> ? T : never;

export type RegisteredTool = ToolDefinition<ZodType>;

export class UnknownToolError extends Error {
  constructor(name: string) {
    super(`Unknown tool "${name}". Registered tools are listed by /status and the tool registry.`);
    this.name = 'UnknownToolError';
  }
}

export class ToolRegistrationError extends Error {
  constructor(detail: string) {
    super(`Invalid tool registration: ${detail}`);
    this.name = 'ToolRegistrationError';
  }
}

/** Decides whether a tool call may run. Deterministic policy code, never a model. */
export type PermissionDecision =
  | { readonly decision: 'allow' }
  | { readonly decision: 'deny'; readonly reason: string; readonly needsUser: boolean };

export type PermissionGate = (
  tool: RegisteredTool,
  args: unknown,
) => PermissionDecision;

export class ToolRegistry {
  private readonly tools = new Map<string, RegisteredTool>();

  register<A extends ZodType>(definition: ToolDefinition<A>): void {
    if (!/^[a-z][a-z0-9-]*$/.test(definition.name)) {
      throw new ToolRegistrationError(`tool name "${definition.name}" must be kebab-case`);
    }
    if (this.tools.has(definition.name)) {
      throw new ToolRegistrationError(`tool "${definition.name}" is already registered`);
    }
    if (definition.timeoutMs <= 0 || definition.maxOutputChars <= 0) {
      throw new ToolRegistrationError(`tool "${definition.name}" needs positive timeout and output limits`);
    }
    this.tools.set(definition.name, definition as unknown as RegisteredTool);
  }

  get(name: string): RegisteredTool {
    const tool = this.tools.get(name);
    if (!tool) throw new UnknownToolError(name);
    return tool;
  }

  has(name: string): boolean {
    return this.tools.has(name);
  }

  list(): readonly RegisteredTool[] {
    return [...this.tools.values()];
  }

  listByCapability(capability: ToolCapability): readonly RegisteredTool[] {
    return this.list().filter((tool) => tool.capability === capability);
  }

  /** Provider-facing tool specs with JSON Schema argument descriptions. */
  toProviderTools(capabilities?: readonly ToolCapability[]): readonly ToolSpec[] {
    return this.list()
      .filter((tool) => !capabilities || capabilities.includes(tool.capability))
      .map((tool) => ({
        name: tool.name,
        description: tool.description,
        parameters: toolJsonSchema(tool),
      }));
  }
}

/** JSON Schema describing a tool's arguments (Zod v4 native). */
export function toolJsonSchema(tool: RegisteredTool): JsonSchemaObject {
  const schema = tool.argsSchema.toJSONSchema({ target: 'draft-2020-12' }) as JsonSchemaObject;
  return { ...schema, additionalProperties: false };
}
