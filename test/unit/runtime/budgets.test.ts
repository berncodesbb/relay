import { describe, expect, it } from 'vitest';
import type { BudgetLimits } from '../../../src/config/schema.js';
import { BudgetExhaustedError, BudgetTracker } from '../../../src/runtime/budgets.js';

function limits(overrides: Partial<BudgetLimits> = {}): BudgetLimits {
  return {
    maxTurns: 10,
    maxModelCalls: 20,
    maxToolCalls: 30,
    maxOutputTokens: 1000,
    wallClockMs: 60_000,
    maxRepeatedActions: 3,
    ...overrides,
  };
}

describe('BudgetTracker', () => {
  it('tracks turns, calls, tokens, and elapsed time', () => {
    const tracker = new BudgetTracker(limits());
    tracker.beginTurn();
    tracker.trackModelCall();
    tracker.trackModelCall();
    tracker.trackToolCall();
    tracker.trackTokens(120);
    const usage = tracker.usage;
    expect(usage.turns).toBe(1);
    expect(usage.modelCalls).toBe(2);
    expect(usage.toolCalls).toBe(1);
    expect(usage.outputTokens).toBe(120);
    expect(usage.elapsedMs).toBeGreaterThanOrEqual(0);
  });

  it('trips assertWithin for each limit with the right budget name', () => {
    const cases: Array<{ setup: (tracker: BudgetTracker) => void; budget: string; limit: number }> = [
      { setup: (t) => t.beginTurn(), budget: 'maxTurns', limit: 1 },
      { setup: (t) => t.trackModelCall(), budget: 'maxModelCalls', limit: 1 },
      { setup: (t) => t.trackToolCall(), budget: 'maxToolCalls', limit: 1 },
      { setup: (t) => t.trackTokens(50), budget: 'maxOutputTokens', limit: 50 },
    ];
    for (const { setup, budget, limit } of cases) {
      const tracker = new BudgetTracker(
        limits({ maxTurns: 1, maxModelCalls: 1, maxToolCalls: 1, maxOutputTokens: 50 }),
      );
      expect(() => tracker.assertWithin()).not.toThrow();
      setup(tracker);
      try {
        tracker.assertWithin();
        expect.unreachable(`${budget} should have tripped`);
      } catch (error) {
        expect(error).toBeInstanceOf(BudgetExhaustedError);
        const exhausted = (error as BudgetExhaustedError).budget;
        expect(exhausted).toBe(budget);
        expect((error as BudgetExhaustedError).message).toContain(budget);
        void limit;
      }
    }
  });

  it('reports wall-clock deadline exhaustion', () => {
    const tracker = new BudgetTracker(limits({ wallClockMs: 10_000 }));
    const elapsed = tracker.usage.elapsedMs;
    void elapsed;
    const shortClock = new BudgetTracker(limits({ wallClockMs: 1 }));
    // Force the deadline by waiting past 1ms.
    return new Promise<void>((resolve) => {
      setTimeout(() => {
        const { exhausted } = shortClock.check();
        expect(exhausted.map((entry) => entry.budget)).toContain('wallClockMs');
        resolve();
      }, 5);
    });
  });

  it('check() reports without throwing and includes actionable details', () => {
    const tracker = new BudgetTracker(limits({ maxToolCalls: 2 }));
    tracker.trackToolCall();
    tracker.trackToolCall();
    const { exhausted } = tracker.check();
    expect(exhausted).toHaveLength(1);
    expect(exhausted[0]?.budget).toBe('maxToolCalls');
    expect(exhausted[0]?.limit).toBe(2);
    expect(exhausted[0]?.detail).toContain('2 tool calls');
  });

  it('fires onExhausted exactly once per budget across repeated checks', () => {
    const seen: Array<[string, string]> = [];
    const tracker = new BudgetTracker(limits({ maxModelCalls: 1 }), {
      onExhausted: (budget, detail) => seen.push([budget, detail]),
    });
    tracker.trackModelCall();
    tracker.check();
    expect(() => tracker.assertWithin()).toThrowError(BudgetExhaustedError);
    tracker.check();
    expect(seen).toHaveLength(1);
    expect(seen[0]?.[0]).toBe('maxModelCalls');
  });

  it('beginTurn/endTurn pair without distorting counts', () => {
    const tracker = new BudgetTracker(limits());
    tracker.beginTurn();
    tracker.endTurn();
    tracker.beginTurn();
    expect(tracker.usage.turns).toBe(2);
  });
});
