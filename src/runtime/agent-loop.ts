/**
 * The agent loop: one model turn, its tool executions, repeat.
 *
 * The loop is provider-agnostic and deterministic in its control flow. It
 * tries routes in order with bounded retries per route, streams text deltas to
 * the caller, validates every tool call against the tool's Zod schema, gates
 * execution through the permission policy, enforces budgets, cancellation,
 * timeouts, and repeated-action limits, and always returns a full transcript
 * plus a machine-readable status so callers can persist resumable state.
 */

import { createHash } from 'node:crypto';
import type { ModelErrorKind, TokenUsage, WorkflowMode } from '../core/events.js';
import type { ToolCapability } from '../core/modes.js';
import { collectTurn } from '../providers/collect.js';
import {
  ProviderFailure,
  type AssistantContent,
  type ModelMessage,
  type ModelRequest,
  type ModelTurnResult,
  type ProviderStreamEvent,
  type StructuredOutputRequest,
} from '../providers/provider.js';
import type { PermissionGate, RegisteredTool, ToolRegistry, ToolResult } from '../tools/registry.js';
import { OperationCanceledError, type CancellationToken } from './cancellation.js';
import { BudgetExhaustedError, type BudgetTracker } from './budgets.js';
import { withRetries } from './retry.js';
import type { ResolvedRoute } from './model-router.js';

export type AgentLoopEvent =
  | { kind: 'model-request'; provider: string; model: string; purpose: string; attempt: number }
  | {
      kind: 'model-response';
      provider: string;
      model: string;
      purpose: string;
      usage: TokenUsage;
      toolCalls: number;
      durationMs: number;
    }
  | {
      kind: 'model-failure';
      provider: string;
      model: string;
      purpose: string;
      errorKind: ModelErrorKind;
      message: string;
      attempts: number;
      durationMs: number;
    }
  | { kind: 'text-delta'; text: string }
  | { kind: 'tool-start'; tool: string; capability: ToolCapability }
  | {
      kind: 'tool-end';
      tool: string;
      capability: ToolCapability;
      ok: boolean;
      durationMs: number;
      summary: string;
      affectedFiles: readonly string[];
    }
  | { kind: 'tool-denied'; tool: string; capability: ToolCapability; reason: string };

export type AgentLoopStatus =
  | 'completed'
  | 'canceled'
  | 'budget-exhausted'
  | 'permission-denied'
  | 'provider-error'
  | 'repeated-action';

export type AgentLoopResult = {
  readonly status: AgentLoopStatus;
  readonly turns: number;
  readonly messages: readonly ModelMessage[];
  readonly finalText: string;
  readonly structuredOutput: { readonly name: string; readonly value: unknown } | null;
  readonly usage: TokenUsage;
  readonly error?: { readonly message: string; readonly kind: ModelErrorKind } | undefined;
};

export type AgentLoopOptions = {
  /** Primary route plus fallbacks, from ModelRouter.routeChain(). */
  readonly routes: readonly ResolvedRoute[];
  readonly system: string;
  readonly purpose: string;
  readonly mode?: WorkflowMode | null;
  readonly initialMessages: readonly ModelMessage[];
  readonly tools: ToolRegistry;
  readonly permittedCapabilities: readonly ToolCapability[];
  readonly permissionGate: PermissionGate;
  readonly budgets: BudgetTracker;
  readonly cancellation: CancellationToken;
  readonly structuredOutput?: StructuredOutputRequest;
  readonly onEvent?: (event: AgentLoopEvent) => void;
  /** Identical tool executions allowed before the loop aborts; default 3. */
  readonly repeatedActionLimit?: number;
  /** Workspace root passed into tool contexts; defaults to process.cwd(). */
  readonly workspaceRoot?: string;
};

const SUMMARY_MAX_CHARS = 400;

/** Minimal local redaction so secrets never reach events or transcripts. */
function redactSecrets(text: string): string {
  return text
    .replace(/sk-ant-[A-Za-z0-9_-]{8,}/g, '[redacted]')
    .replace(/sk-[A-Za-z0-9_-]{8,}/g, '[redacted]')
    .replace(/\bBearer\s+[A-Za-z0-9._~+/=-]{8,}/gi, 'Bearer [redacted]');
}

type ToolExecution =
  | { status: 'ok'; result: ToolResult; durationMs: number }
  | { status: 'timeout'; durationMs: number }
  | { status: 'canceled' };

export async function runAgentLoop(options: AgentLoopOptions): Promise<AgentLoopResult> {
  const emit = (event: AgentLoopEvent): void => {
    options.onEvent?.(event);
  };

  const messages: ModelMessage[] = [...options.initialMessages];
  const totals: { inputTokens: number; outputTokens: number } = { inputTokens: 0, outputTokens: 0 };
  const repeatCounts = new Map<string, number>();
  const repeatedActionLimit = options.repeatedActionLimit ?? 3;
  const toolSpecs = options.tools.toProviderTools(options.permittedCapabilities);
  let turns = 0;
  let finalText = '';
  let structuredOutput: { name: string; value: unknown } | null = null;

  const buildResult = (
    status: AgentLoopStatus,
    error?: { message: string; kind: ModelErrorKind },
  ): AgentLoopResult => ({
    status,
    turns,
    messages,
    finalText,
    structuredOutput,
    usage: totals,
    ...(error !== undefined ? { error } : {}),
  });

  /** Stream pass-through that forwards text deltas to the caller live. */
  async function* tapDeltas(
    stream: AsyncIterable<ProviderStreamEvent>,
  ): AsyncIterable<ProviderStreamEvent> {
    for await (const event of stream) {
      if (event.type === 'text-delta') emit({ kind: 'text-delta', text: event.text });
      yield event;
    }
  }

  /** One model call on one route, with bounded retries inside the route. */
  async function callModel(route: ResolvedRoute): Promise<ModelTurnResult> {
    let attempts = 0;
    const startedAt = Date.now();
    try {
      return await withRetries(
        async (attempt) => {
          attempts = attempt;
          emit({
            kind: 'model-request',
            provider: route.providerName,
            model: route.model,
            purpose: options.purpose,
            attempt,
          });
          const callStartedAt = Date.now();
          const request: ModelRequest = {
            model: route.model,
            system: options.system,
            messages,
            ...(toolSpecs.length > 0 ? { tools: toolSpecs } : {}),
            ...(options.structuredOutput !== undefined
              ? { structuredOutput: options.structuredOutput }
              : {}),
            ...(route.limits?.maxOutputTokens !== undefined
              ? { maxOutputTokens: route.limits.maxOutputTokens }
              : {}),
            ...(route.limits?.temperature !== undefined
              ? { temperature: route.limits.temperature }
              : {}),
            purpose: options.purpose,
            signal: options.cancellation.signal,
          };
          const result = await collectTurn(tapDeltas(route.provider.stream(request)));
          emit({
            kind: 'model-response',
            provider: route.providerName,
            model: route.model,
            purpose: options.purpose,
            usage: result.usage,
            toolCalls: result.toolCalls.length,
            durationMs: Date.now() - callStartedAt,
          });
          options.budgets.trackModelCall();
          options.budgets.trackTokens(result.usage.outputTokens);
          totals.inputTokens += result.usage.inputTokens;
          totals.outputTokens += result.usage.outputTokens;
          return result;
        },
        { signal: options.cancellation.signal },
      );
    } catch (error) {
      if (error instanceof ProviderFailure) {
        emit({
          kind: 'model-failure',
          provider: route.providerName,
          model: route.model,
          purpose: options.purpose,
          errorKind: error.kind,
          message: redactSecrets(error.message).slice(0, 2000),
          attempts: Math.max(1, attempts),
          durationMs: Date.now() - startedAt,
        });
      }
      throw error;
    }
  }

  /** Execute a validated tool call bounded by its timeout and cancellation. */
  async function runTool(tool: RegisteredTool, args: unknown): Promise<ToolExecution> {
    const startedAt = Date.now();
    let timer: ReturnType<typeof setTimeout> | undefined;
    let disposeCancel: (() => void) | undefined;
    const context = {
      workspaceRoot: options.workspaceRoot ?? process.cwd(),
      signal: options.cancellation.signal,
      log: () => undefined,
    };
    try {
      const toolPromise = tool.execute(args, context).then((result) => ({
        status: 'ok' as const,
        result,
      }));
      // If timeout/cancellation wins the race, a later tool rejection must not
      // surface as an unhandled rejection.
      toolPromise.catch(() => undefined);
      const execution = await Promise.race([
        toolPromise,
        new Promise<{ status: 'timeout' }>((resolve) => {
          timer = setTimeout(() => resolve({ status: 'timeout' }), tool.timeoutMs);
        }),
        new Promise<{ status: 'canceled' }>((resolve) => {
          disposeCancel = options.cancellation.onCancel(() => resolve({ status: 'canceled' }));
        }),
      ]);
      if (execution.status === 'ok') {
        return { status: 'ok', result: execution.result, durationMs: Date.now() - startedAt };
      }
      return { status: execution.status, durationMs: Date.now() - startedAt };
    } catch (error) {
      // Tool failures become error results so the model can react to them.
      const message =
        error instanceof Error ? error.message : `tool "${tool.name}" failed unexpectedly`;
      return {
        status: 'ok',
        durationMs: Date.now() - startedAt,
        result: {
          ok: false,
          outcome: 'error',
          output: redactSecrets(message).slice(0, tool.maxOutputChars),
          affectedFiles: [],
        },
      };
    } finally {
      if (timer !== undefined) clearTimeout(timer);
      disposeCancel?.();
    }
  }

  // Main loop: one iteration = one turn (one model call + its tool executions).
  for (;;) {
    try {
      options.budgets.assertWithin();
      options.cancellation.throwIfCanceled();
    } catch (error) {
      if (error instanceof OperationCanceledError) {
        return buildResult('canceled', { message: error.message, kind: 'canceled' });
      }
      if (error instanceof BudgetExhaustedError) {
        return buildResult('budget-exhausted', { message: error.message, kind: 'unknown' });
      }
      throw error;
    }

    options.budgets.beginTurn();
    turns += 1;

    // Try each route in order; a route gets bounded retries before failing over.
    let turn: ModelTurnResult | undefined;
    let lastFailure: ProviderFailure | null = null;
    for (const route of options.routes) {
      try {
        turn = await callModel(route);
        lastFailure = null;
        break;
      } catch (error) {
        if (error instanceof ProviderFailure) {
          if (error.kind === 'canceled') {
            return buildResult('canceled', { message: redactSecrets(error.message), kind: 'canceled' });
          }
          lastFailure = error;
          continue;
        }
        throw error;
      }
    }

    if (turn === undefined) {
      const failure =
        lastFailure ??
        new ProviderFailure('unknown', 'no model route was available for this agent loop');
      return buildResult('provider-error', {
        message: redactSecrets(failure.message),
        kind: failure.kind,
      });
    }

    const assistantContent: AssistantContent[] = [];
    if (turn.text.length > 0) assistantContent.push({ type: 'text', text: turn.text });
    for (const call of turn.toolCalls) assistantContent.push({ type: 'tool-call', call });
    messages.push({ role: 'assistant', content: assistantContent });
    if (turn.text.length > 0) finalText = turn.text;
    if (turn.structuredOutput !== null) structuredOutput = turn.structuredOutput;

    if (turn.toolCalls.length === 0) {
      options.budgets.endTurn();
      if (turn.finishReason === 'length') {
        finalText = `${finalText}\n[Output truncated: the model reached its output token limit before finishing.]`;
      }
      return buildResult('completed');
    }

    for (const call of turn.toolCalls) {
      try {
        options.cancellation.throwIfCanceled();
      } catch (error) {
        if (error instanceof OperationCanceledError) {
          return buildResult('canceled', { message: error.message, kind: 'canceled' });
        }
        throw error;
      }

      let tool: RegisteredTool;
      try {
        tool = options.tools.get(call.name);
      } catch {
        // Unknown tool (model hallucinated a name): tell it what exists.
        options.budgets.trackToolCall();
        messages.push({
          role: 'tool-result',
          toolCallId: call.id,
          content:
            `Unknown tool "${call.name}". Available tools: ` +
            `${options.tools.list().map((entry) => entry.name).join(', ')}.`,
        });
        continue;
      }

      const decision = options.permissionGate(tool, call.arguments);
      if (decision.decision === 'deny') {
        emit({ kind: 'tool-denied', tool: tool.name, capability: tool.capability, reason: decision.reason });
        options.budgets.endTurn();
        return buildResult('permission-denied', {
          message: `Tool "${tool.name}" was denied by the permission gate: ${decision.reason}`,
          kind: 'unknown',
        });
      }

      const parsed = tool.argsSchema.safeParse(call.arguments);
      if (!parsed.success) {
        // Invalid arguments: report the validation issues so the model corrects.
        options.budgets.trackToolCall();
        const issues = parsed.error.issues
          .map((issue) => `${issue.path.join('.') || '(root)'}: ${issue.message}`)
          .join('; ');
        messages.push({
          role: 'tool-result',
          toolCallId: call.id,
          content:
            `Invalid arguments for tool "${tool.name}": ${issues}. ` +
            'Call the tool again with arguments matching its schema.',
        });
        continue;
      }

      const hash = createHash('sha1')
        .update(`${tool.name}:${JSON.stringify(call.arguments)}`)
        .digest('hex');
      const seen = repeatCounts.get(hash) ?? 0;
      if (seen >= repeatedActionLimit) {
        options.budgets.endTurn();
        return buildResult('repeated-action', {
          message:
            `Tool "${tool.name}" was executed ${seen} times with identical arguments, ` +
            `which exceeds the repeated-action limit of ${repeatedActionLimit}. ` +
            'Change the arguments, choose a different tool, or finish without repeating the call.',
          kind: 'unknown',
        });
      }
      repeatCounts.set(hash, seen + 1);

      emit({ kind: 'tool-start', tool: tool.name, capability: tool.capability });
      options.budgets.trackToolCall();

      const execution = await runTool(tool, parsed.data);
      if (execution.status === 'canceled') {
        return buildResult('canceled', {
          message: `Operation canceled: ${options.cancellation.reason ?? 'canceled'} (during tool "${tool.name}")`,
          kind: 'canceled',
        });
      }
      if (execution.status === 'timeout') {
        const content = `Tool "${tool.name}" timed out after ${tool.timeoutMs}ms and was abandoned.`;
        emit({
          kind: 'tool-end',
          tool: tool.name,
          capability: tool.capability,
          ok: false,
          durationMs: execution.durationMs,
          summary: content,
          affectedFiles: [],
        });
        messages.push({ role: 'tool-result', toolCallId: call.id, content });
      } else {
        emit({
          kind: 'tool-end',
          tool: tool.name,
          capability: tool.capability,
          ok: execution.result.ok,
          durationMs: execution.durationMs,
          summary: redactSecrets(execution.result.output).slice(0, SUMMARY_MAX_CHARS),
          affectedFiles: execution.result.affectedFiles,
        });
        messages.push({
          role: 'tool-result',
          toolCallId: call.id,
          content: redactSecrets(execution.result.output),
        });
      }

      // Stop before further work once any budget is at its limit.
      const { exhausted } = options.budgets.check();
      const firstExhausted = exhausted[0];
      if (firstExhausted !== undefined) {
        options.budgets.endTurn();
        return buildResult('budget-exhausted', {
          message: `Budget "${firstExhausted.budget}" exhausted: ${firstExhausted.detail}`,
          kind: 'unknown',
        });
      }
    }

    options.budgets.endTurn();
  }
}
