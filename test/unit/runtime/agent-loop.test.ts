import { describe, expect, it } from 'vitest';
import { z } from 'zod';
import type { BudgetLimits } from '../../../src/config/schema.js';
import { FakeProvider } from '../../../src/providers/fake.js';
import type { ModelProvider, ProviderName } from '../../../src/providers/provider.js';
import { ToolRegistry, type PermissionGate } from '../../../src/tools/registry.js';
import { runAgentLoop, type AgentLoopEvent, type AgentLoopOptions } from '../../../src/runtime/agent-loop.js';
import { BudgetTracker } from '../../../src/runtime/budgets.js';
import { CancellationToken } from '../../../src/runtime/cancellation.js';
import type { ResolvedRoute } from '../../../src/runtime/model-router.js';

const FULL_LIMITS: BudgetLimits = {
  maxTurns: 24,
  maxModelCalls: 48,
  maxToolCalls: 96,
  maxOutputTokens: 400_000,
  wallClockMs: 1_800_000,
  maxRepeatedActions: 3,
};

const allowAll: PermissionGate = () => ({ decision: 'allow' });

function route(
  provider: ModelProvider,
  providerName: ProviderName,
  model = 'fake-standard',
  limits?: ResolvedRoute['limits'],
): ResolvedRoute {
  return { provider, providerName, model, ...(limits !== undefined ? { limits } : {}) };
}

function makeRegistry(): ToolRegistry {
  const registry = new ToolRegistry();
  registry.register({
    name: 'echo',
    title: 'Echo',
    description: 'Echoes text back to the caller.',
    capability: 'read',
    argsSchema: z.object({ text: z.string() }),
    timeoutMs: 5_000,
    maxOutputChars: 4_000,
    execute: async (args) => ({
      ok: true,
      outcome: 'ok' as const,
      output: `Echo: ${args.text}`,
      affectedFiles: [],
    }),
  });
  registry.register({
    name: 'record',
    title: 'Record',
    description: 'Records a number to a file.',
    capability: 'write',
    argsSchema: z.object({ value: z.number() }),
    timeoutMs: 5_000,
    maxOutputChars: 4_000,
    execute: async (args) => ({
      ok: true,
      outcome: 'ok' as const,
      output: `Recorded ${args.value}`,
      data: { value: args.value },
      affectedFiles: ['out.txt'],
    }),
  });
  return registry;
}

function baseOptions(overrides: {
  provider?: FakeProvider;
  routes?: readonly ResolvedRoute[];
  limits?: Partial<BudgetLimits>;
  gate?: PermissionGate;
  repeatedActionLimit?: number;
  structuredOutput?: AgentLoopOptions['structuredOutput'];
}): {
  options: AgentLoopOptions;
  events: AgentLoopEvent[];
  budgets: BudgetTracker;
  cancellation: { token: CancellationToken; cancel: (reason?: string) => void };
} {
  const provider = overrides.provider ?? new FakeProvider([], { text: 'done' });
  const events: AgentLoopEvent[] = [];
  const budgets = new BudgetTracker({ ...FULL_LIMITS, ...overrides.limits });
  const cancellation = CancellationToken.create();
  const options: AgentLoopOptions = {
    routes: overrides.routes ?? [route(provider, 'fake')],
    system: 'test system prompt',
    purpose: 'test',
    initialMessages: [{ role: 'user', content: 'please proceed' }],
    tools: makeRegistry(),
    permittedCapabilities: ['read', 'write', 'execute'],
    permissionGate: overrides.gate ?? allowAll,
    budgets,
    cancellation: cancellation.token,
    ...(overrides.repeatedActionLimit !== undefined
      ? { repeatedActionLimit: overrides.repeatedActionLimit }
      : {}),
    ...(overrides.structuredOutput !== undefined
      ? { structuredOutput: overrides.structuredOutput }
      : {}),
    onEvent: (event) => events.push(event),
  };
  return { options, events, budgets, cancellation };
}

describe('runAgentLoop', () => {
  it('completes after a tool round-trip with a full transcript and usage totals', async () => {
    const provider = new FakeProvider(
      [{ match: { purpose: 'test', maxCall: 1 }, response: { toolCalls: [{ name: 'echo', args: { text: 'hi' } }] } }],
      { text: 'all done' },
    );
    const { options, events, budgets } = baseOptions({ provider });

    const result = await runAgentLoop(options);

    expect(result.status).toBe('completed');
    expect(result.turns).toBe(2);
    expect(result.finalText).toBe('all done');
    expect(result.usage).toEqual({ inputTokens: 200, outputTokens: 100 });
    expect(budgets.usage.modelCalls).toBe(2);

    // Transcript: user, assistant(tool-call), tool-result, assistant(final).
    expect(result.messages).toHaveLength(4);
    expect(result.messages[0]).toEqual({ role: 'user', content: 'please proceed' });
    const assistant = result.messages[1];
    if (assistant === undefined || assistant.role !== 'assistant') {
      throw new Error('expected assistant message');
    }
    expect(assistant.content).toHaveLength(1);
    expect(assistant.content[0]?.type).toBe('tool-call');
    if (assistant.content[0]?.type === 'tool-call') {
      expect(assistant.content[0].call.name).toBe('echo');
    }
    expect(result.messages[2]).toMatchObject({
      role: 'tool-result',
      content: 'Echo: hi',
    });
    expect(result.messages[3]).toMatchObject({ role: 'assistant', content: [{ type: 'text', text: 'all done' }] });

    const kinds = events.map((event) => event.kind);
    expect(kinds).toContain('model-request');
    expect(kinds).toContain('model-response');
    expect(kinds).toContain('tool-start');
    expect(kinds.filter((kind) => kind === 'tool-end')).toHaveLength(1);
    const toolEnd = events.find((event) => event.kind === 'tool-end');
    if (toolEnd?.kind === 'tool-end') {
      expect(toolEnd.tool).toBe('echo');
      expect(toolEnd.capability).toBe('read');
      expect(toolEnd.ok).toBe(true);
      expect(toolEnd.summary).toBe('Echo: hi');
      expect(toolEnd.affectedFiles).toEqual([]);
    }
    expect(events.filter((event) => event.kind === 'text-delta').length).toBeGreaterThan(0);
  });

  it('stops with permission-denied when the gate rejects a tool', async () => {
    const provider = new FakeProvider(
      [{ match: { maxCall: 1 }, response: { toolCalls: [{ name: 'record', args: { value: 7 } }] } }],
      { text: 'never reached' },
    );
    const { options, events } = baseOptions({
      provider,
      gate: (tool) =>
        tool.capability === 'write'
          ? { decision: 'deny', reason: 'writes require user approval in this mode', needsUser: true }
          : { decision: 'allow' },
    });

    const result = await runAgentLoop(options);

    expect(result.status).toBe('permission-denied');
    expect(result.error?.message).toContain('record');
    expect(result.error?.message).toContain('writes require user approval');
    const denied = events.find((event) => event.kind === 'tool-denied');
    expect(denied).toBeDefined();
    if (denied?.kind === 'tool-denied') {
      expect(denied.tool).toBe('record');
      expect(denied.capability).toBe('write');
      expect(denied.reason).toContain('approval');
    }
    // No tool result was produced after the denial.
    expect(result.messages.filter((message) => message.role === 'tool-result')).toHaveLength(0);
  });

  it('answers invalid tool arguments with a corrective tool-result, then completes', async () => {
    const provider = new FakeProvider(
      [{ match: { maxCall: 1 }, response: { toolCalls: [{ name: 'echo', args: { text: 42 } }] } }],
      { text: 'recovered' },
    );
    const { options } = baseOptions({ provider });

    const result = await runAgentLoop(options);

    expect(result.status).toBe('completed');
    const toolResult = result.messages.find((message) => message.role === 'tool-result');
    expect(toolResult).toBeDefined();
    if (toolResult?.role === 'tool-result') {
      expect(toolResult.content).toContain('Invalid arguments for tool "echo"');
      expect(toolResult.content).toContain('text');
      expect(toolResult.content).toContain('schema');
    }
    expect(result.finalText).toBe('recovered');
  });

  it('reports unknown tools back to the model instead of crashing', async () => {
    const provider = new FakeProvider(
      [{ match: { maxCall: 1 }, response: { toolCalls: [{ name: 'nonexistent-tool', args: {} }] } }],
      { text: 'moved on' },
    );
    const { options } = baseOptions({ provider });

    const result = await runAgentLoop(options);

    expect(result.status).toBe('completed');
    const toolResult = result.messages.find((message) => message.role === 'tool-result');
    if (toolResult?.role === 'tool-result') {
      expect(toolResult.content).toContain('Unknown tool "nonexistent-tool"');
      expect(toolResult.content).toContain('echo');
    }
  });

  it('stops mid-loop when the tool-call budget is exhausted', async () => {
    const provider = new FakeProvider(
      [],
      {
        toolCalls: [
          { name: 'echo', args: { text: 'first' } },
          { name: 'echo', args: { text: 'second' } },
        ],
      },
    );
    const { options, budgets } = baseOptions({ provider, limits: { maxToolCalls: 1 } });

    const result = await runAgentLoop(options);

    expect(result.status).toBe('budget-exhausted');
    expect(result.error?.message).toContain('maxToolCalls');
    expect(budgets.usage.toolCalls).toBe(1);
    expect(result.messages.filter((message) => message.role === 'tool-result')).toHaveLength(1);
  });

  it('stops before a new turn when the turn budget is exhausted', async () => {
    const provider = new FakeProvider(
      [{ match: { maxCall: 1 }, response: { toolCalls: [{ name: 'echo', args: { text: 'one' } }] } }],
      { text: 'final' },
    );
    const { options } = baseOptions({ provider, limits: { maxTurns: 1 } });

    const result = await runAgentLoop(options);

    expect(result.status).toBe('budget-exhausted');
    expect(result.error?.message).toContain('maxTurns');
  });

  it('returns canceled when cancellation fires during a model call', async () => {
    const provider = new FakeProvider([], { text: 'slow', delayMs: 30_000 });
    const { options, cancellation } = baseOptions({ provider });
    setTimeout(() => cancellation.cancel('user interrupt'), 20);

    const result = await runAgentLoop(options);

    expect(result.status).toBe('canceled');
    expect(result.error?.kind).toBe('canceled');
    expect(result.error?.message).toContain('user interrupt');
  });

  it('returns canceled when cancellation fires during a tool execution', async () => {
    const registry = makeRegistry();
    registry.register({
      name: 'slow-echo',
      title: 'Slow echo',
      description: 'Takes a while.',
      capability: 'read',
      argsSchema: z.object({ text: z.string() }),
      timeoutMs: 30_000,
      maxOutputChars: 100,
      execute: async (args) => {
        await new Promise((resolve) => setTimeout(resolve, 300));
        return { ok: true, outcome: 'ok' as const, output: `Slow: ${args.text}`, affectedFiles: [] };
      },
    });
    const provider = new FakeProvider(
      [{ match: { maxCall: 1 }, response: { toolCalls: [{ name: 'slow-echo', args: { text: 'x' } }] } }],
      { text: 'never' },
    );
    const { options, cancellation } = baseOptions({ provider });
    (options as { tools: ToolRegistry }).tools = registry;
    setTimeout(() => cancellation.cancel('user interrupt'), 20);

    const result = await runAgentLoop(options);

    expect(result.status).toBe('canceled');
    expect(result.error?.kind).toBe('canceled');
  });

  it('aborts on repeated identical tool executions with an actionable message', async () => {
    const provider = new FakeProvider([], {
      toolCalls: [{ name: 'echo', args: { text: 'same' } }],
    });
    const { options } = baseOptions({ provider, repeatedActionLimit: 2 });

    const result = await runAgentLoop(options);

    expect(result.status).toBe('repeated-action');
    expect(result.error?.message).toContain('echo');
    expect(result.error?.message).toContain('repeated-action limit');
  });

  it('fails over to the next route after the primary exhausts its retries', async () => {
    const primary = new FakeProvider(
      [{ match: {}, response: { error: { kind: 'server', retryable: true, message: 'primary down', failTimes: 10 } } }],
      { text: 'never' },
    );
    const fallback = new FakeProvider([], { text: 'rescued' });
    const { options, events, budgets } = baseOptions({
      routes: [route(primary, 'openai', 'gpt-5.2'), route(fallback, 'anthropic', 'claude-sonnet-5-5')],
    });

    const result = await runAgentLoop(options);

    expect(result.status).toBe('completed');
    expect(result.finalText).toBe('rescued');
    const failure = events.find((event) => event.kind === 'model-failure');
    if (failure?.kind === 'model-failure') {
      expect(failure.provider).toBe('openai');
      expect(failure.model).toBe('gpt-5.2');
      expect(failure.errorKind).toBe('server');
      expect(failure.attempts).toBe(3);
      expect(failure.message).toBe('primary down');
    }
    const requests = events.filter((event) => event.kind === 'model-request');
    expect(requests).toHaveLength(4); // 3 on primary, 1 on fallback
    if (requests[3]?.kind === 'model-request') {
      expect(requests[3].provider).toBe('anthropic');
      expect(requests[3].attempt).toBe(1);
    }
    expect(budgets.usage.modelCalls).toBe(1); // failures track no usage
    expect(result.usage).toEqual({ inputTokens: 100, outputTokens: 50 });
  });

  it('returns provider-error with a redacted message when every route fails', async () => {
    const primary = new FakeProvider(
      [{ match: {}, response: { error: { kind: 'auth', retryable: false, message: 'key sk-abcdefghijklmnop failed', failTimes: 10 } } }],
      { text: 'never' },
    );
    const secondary = new FakeProvider(
      [{ match: {}, response: { error: { kind: 'server', retryable: true, message: 'also down', failTimes: 10 } } }],
      { text: 'never' },
    );
    const { options, events } = baseOptions({
      routes: [route(primary, 'openai'), route(secondary, 'anthropic')],
    });

    const result = await runAgentLoop(options);

    expect(result.status).toBe('provider-error');
    expect(result.error?.kind).toBe('server'); // last route's failure wins the report
    expect(result.error?.message).toBe('also down');
    const failures = events.filter((event) => event.kind === 'model-failure');
    expect(failures).toHaveLength(2);
    const authFailure = failures[0];
    if (authFailure?.kind === 'model-failure') {
      expect(authFailure.message).toContain('[redacted]');
      expect(authFailure.message).not.toContain('sk-abcdefghijklmnop');
    }
  });

  it('captures structured output from the final turn', async () => {
    const provider = new FakeProvider([], {
      structuredOutput: { mode: 'planning' },
      text: '{"mode":"planning"}',
    });
    const { options } = baseOptions({
      provider,
      structuredOutput: { name: 'intent-proposal', schema: { type: 'object' } },
    });

    const result = await runAgentLoop(options);

    expect(result.status).toBe('completed');
    expect(result.structuredOutput).toEqual({
      name: 'intent-proposal',
      value: { mode: 'planning' },
    });
  });

  it('notes truncation in finalText when the model hits the token limit', async () => {
    const provider = new FakeProvider([], { text: 'partial answer', finishReason: 'length' });
    const { options } = baseOptions({ provider });

    const result = await runAgentLoop(options);

    expect(result.status).toBe('completed');
    expect(result.finalText).toContain('partial answer');
    expect(result.finalText).toContain('truncated');
  });

  it('applies per-route limits to model requests', async () => {
    const provider = new FakeProvider([], { text: 'limited' });
    const { options } = baseOptions({
      provider,
      routes: [route(provider, 'fake', 'fake-standard', { maxOutputTokens: 1234, temperature: 0.1 })],
    });

    await runAgentLoop(options);

    expect(provider.callLog[0]?.model).toBe('fake-standard');
  });

  it('converts tool timeouts into error tool-results and keeps going', async () => {
    const registry = makeRegistry();
    registry.register({
      name: 'hang',
      title: 'Hang',
      description: 'Hangs past its timeout.',
      capability: 'execute',
      argsSchema: z.object({}),
      timeoutMs: 30,
      maxOutputChars: 100,
      execute: async () => {
        await new Promise((resolve) => setTimeout(resolve, 300));
        return { ok: true, outcome: 'ok' as const, output: 'too late', affectedFiles: [] };
      },
    });
    const provider = new FakeProvider(
      [{ match: { maxCall: 1 }, response: { toolCalls: [{ name: 'hang', args: {} }] } }],
      { text: 'finished anyway' },
    );
    const { options, events } = baseOptions({ provider });
    // Swap in the registry that knows the hang tool.
    (options as { tools: ToolRegistry }).tools = registry;

    const result = await runAgentLoop(options);

    expect(result.status).toBe('completed');
    const toolResult = result.messages.find((message) => message.role === 'tool-result');
    if (toolResult?.role === 'tool-result') {
      expect(toolResult.content).toContain('timed out');
    }
    const toolEnd = events.find((event) => event.kind === 'tool-end');
    if (toolEnd?.kind === 'tool-end') {
      expect(toolEnd.ok).toBe(false);
      expect(toolEnd.summary).toContain('timed out');
    }
  });

  it('redacts secrets in tool output summaries and transcripts', async () => {
    const registry = makeRegistry();
    registry.register({
      name: 'leaky',
      title: 'Leaky',
      description: 'Returns a secret.',
      capability: 'read',
      argsSchema: z.object({}),
      timeoutMs: 5_000,
      maxOutputChars: 10_000,
      execute: async () => ({
        ok: true,
        outcome: 'ok' as const,
        output: 'token sk-abcdefghijklmnopq used',
        affectedFiles: [],
      }),
    });
    const provider = new FakeProvider(
      [{ match: { maxCall: 1 }, response: { toolCalls: [{ name: 'leaky', args: {} }] } }],
      { text: 'done' },
    );
    const { options, events } = baseOptions({ provider });
    (options as { tools: ToolRegistry }).tools = registry;

    const result = await runAgentLoop(options);

    const toolEnd = events.find((event) => event.kind === 'tool-end');
    if (toolEnd?.kind === 'tool-end') {
      expect(toolEnd.summary).not.toContain('sk-abcdefghijklmnopq');
      expect(toolEnd.summary).toContain('[redacted]');
    }
    const toolResult = result.messages.find((message) => message.role === 'tool-result');
    if (toolResult?.role === 'tool-result') {
      expect(toolResult.content).toContain('[redacted]');
    }
  });

  it('reports tool execution errors as error tool-results without stopping the loop', async () => {
    const registry = makeRegistry();
    registry.register({
      name: 'broken',
      title: 'Broken',
      description: 'Always throws.',
      capability: 'read',
      argsSchema: z.object({}),
      timeoutMs: 5_000,
      maxOutputChars: 1_000,
      execute: async () => {
        throw new Error('disk exploded');
      },
    });
    const provider = new FakeProvider(
      [{ match: { maxCall: 1 }, response: { toolCalls: [{ name: 'broken', args: {} }] } }],
      { text: 'recovered' },
    );
    const { options, events } = baseOptions({ provider });
    (options as { tools: ToolRegistry }).tools = registry;

    const result = await runAgentLoop(options);

    expect(result.status).toBe('completed');
    const toolResult = result.messages.find((message) => message.role === 'tool-result');
    if (toolResult?.role === 'tool-result') {
      expect(toolResult.content).toContain('disk exploded');
    }
    const toolEnd = events.find((event) => event.kind === 'tool-end');
    if (toolEnd?.kind === 'tool-end') {
      expect(toolEnd.ok).toBe(false);
      expect(toolEnd.durationMs).toBeGreaterThanOrEqual(0);
    }
  });
});
