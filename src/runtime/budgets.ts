/**
 * Run-time budget tracking.
 *
 * The agent loop consults a BudgetTracker before every turn and after every
 * tool execution so a runaway model cannot burn unbounded turns, calls,
 * tokens, or wall-clock time. Checks are non-destructive: `check()` reports,
 * `assertWithin()` throws, and `onExhausted` is invoked exactly once per
 * budget so the caller can record a single `budget.exhausted` event.
 */

import type { BudgetLimits } from '../config/schema.js';

export type BudgetExhaustion = {
  readonly budget: string;
  readonly limit: number;
  readonly detail: string;
};

export class BudgetExhaustedError extends Error {
  constructor(
    readonly budget: string,
    detail: string,
  ) {
    super(`Budget "${budget}" exhausted: ${detail}`);
    this.name = 'BudgetExhaustedError';
  }
}

export type BudgetUsage = {
  readonly turns: number;
  readonly modelCalls: number;
  readonly toolCalls: number;
  readonly outputTokens: number;
  readonly elapsedMs: number;
};

export type BudgetTrackerOptions = {
  /** Called exactly once per budget when it first registers as exhausted. */
  readonly onExhausted?: (budget: string, detail: string) => void;
};

export class BudgetTracker {
  private readonly limits: BudgetLimits;
  private readonly onExhausted: ((budget: string, detail: string) => void) | undefined;
  private readonly startedAt: number;
  private readonly notified = new Set<string>();
  private turns = 0;
  private modelCalls = 0;
  private toolCalls = 0;
  private outputTokens = 0;

  constructor(limits: BudgetLimits, options: BudgetTrackerOptions = {}) {
    this.limits = limits;
    this.onExhausted = options.onExhausted;
    this.startedAt = Date.now();
  }

  get usage(): BudgetUsage {
    return {
      turns: this.turns,
      modelCalls: this.modelCalls,
      toolCalls: this.toolCalls,
      outputTokens: this.outputTokens,
      elapsedMs: Date.now() - this.startedAt,
    };
  }

  beginTurn(): void {
    this.turns += 1;
  }

  endTurn(): void {
    // Turns are counted at beginTurn; endTurn marks the boundary for callers
    // that want explicit pairing and is intentionally idempotent-free.
  }

  trackModelCall(): void {
    this.modelCalls += 1;
  }

  trackToolCall(): void {
    this.toolCalls += 1;
  }

  /** Add output tokens consumed by a completed model call. */
  trackTokens(count: number): void {
    if (count > 0) this.outputTokens += count;
  }

  /** Report every exhausted budget without throwing. */
  check(): { exhausted: readonly BudgetExhaustion[] } {
    const { turns, modelCalls, toolCalls, outputTokens, elapsedMs } = this.usage;
    const exhausted: BudgetExhaustion[] = [];

    if (turns >= this.limits.maxTurns) {
      exhausted.push({
        budget: 'maxTurns',
        limit: this.limits.maxTurns,
        detail: `${turns} turns started; the run may not start another turn`,
      });
    }
    if (modelCalls >= this.limits.maxModelCalls) {
      exhausted.push({
        budget: 'maxModelCalls',
        limit: this.limits.maxModelCalls,
        detail: `${modelCalls} model calls made; no further model calls are permitted`,
      });
    }
    if (toolCalls >= this.limits.maxToolCalls) {
      exhausted.push({
        budget: 'maxToolCalls',
        limit: this.limits.maxToolCalls,
        detail: `${toolCalls} tool calls made; no further tool calls are permitted`,
      });
    }
    if (outputTokens >= this.limits.maxOutputTokens) {
      exhausted.push({
        budget: 'maxOutputTokens',
        limit: this.limits.maxOutputTokens,
        detail: `${outputTokens} output tokens consumed`,
      });
    }
    if (elapsedMs >= this.limits.wallClockMs) {
      exhausted.push({
        budget: 'wallClockMs',
        limit: this.limits.wallClockMs,
        detail: `${elapsedMs}ms elapsed of a ${this.limits.wallClockMs}ms deadline`,
      });
    }

    for (const entry of exhausted) {
      if (!this.notified.has(entry.budget)) {
        this.notified.add(entry.budget);
        this.onExhausted?.(entry.budget, entry.detail);
      }
    }

    return { exhausted };
  }

  /** Throw `BudgetExhaustedError` naming the first exhausted budget, if any. */
  assertWithin(): void {
    const { exhausted } = this.check();
    const first = exhausted[0];
    if (first !== undefined) {
      throw new BudgetExhaustedError(first.budget, first.detail);
    }
  }
}
