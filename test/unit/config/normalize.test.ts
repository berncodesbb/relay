/**
 * Routing normalization tests.
 *
 * `normalizeRouting` is the seam between raw configuration (role-name
 * references or legacy inline objects) and the runtime routing contract: it
 * expands every catalog role to its fallback chain, resolves every routing
 * key to a chain without fabricating role names for inline routes, and keeps
 * the historical first-route fallback for non-mode purposes. It is pure and
 * deterministic; all fixtures here are hand-built or the built-in defaults.
 */

import { describe, expect, it } from 'vitest';
import { DEFAULT_CONFIG } from '../../../src/config/defaults.js';
import { normalizeRouting } from '../../../src/config/load.js';
import type { ModelRoleConfig, RelayConfig, RoutingTable } from '../../../src/config/schema.js';

function configWith(
  routing: RoutingTable,
  models?: Record<string, ModelRoleConfig> | undefined,
): RelayConfig {
  return {
    version: 1,
    providers: {
      openai: { apiKeyEnv: 'OPENAI_API_KEY' },
      anthropic: { apiKeyEnv: 'ANTHROPIC_API_KEY' },
    },
    routing,
    ...(models !== undefined ? { models } : {}),
    budgets: {
      maxTurns: 24,
      maxModelCalls: 48,
      maxToolCalls: 96,
      maxOutputTokens: 400_000,
      wallClockMs: 1_800_000,
      maxRepeatedActions: 3,
    },
    autonomy: 'next-checkpoint',
    tools: { commandTimeoutMs: 120_000, maxSearchResults: 200, maxFileBytes: 2_000_000 },
  };
}

const INLINE_ROUTES: RoutingTable = {
  discovery: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
  specification: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
  'technical-design': { provider: 'anthropic', model: 'claude-opus-5-5' },
  planning: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
  implementation: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
  verification: { provider: 'openai', model: 'gpt-5.2' },
  delivery: { provider: 'anthropic', model: 'claude-haiku-4-5' },
};

describe('normalizing the built-in defaults', () => {
  it('routes intent to the lightweight fast role', () => {
    const routing = normalizeRouting(DEFAULT_CONFIG);
    expect(routing.purposeChains.get('intent')).toEqual([
      { role: 'fast', provider: 'anthropic', model: 'claude-haiku-4-5', limits: undefined },
    ]);
  });

  it('resolves every workflow mode to today\'s provider/model with role names', () => {
    const routing = normalizeRouting(DEFAULT_CONFIG);
    const reasoning = { role: 'reasoning', provider: 'anthropic', model: 'claude-sonnet-5-5', limits: undefined };
    const expected: Record<string, unknown[]> = {
      discovery: [reasoning],
      specification: [reasoning],
      planning: [reasoning],
      'technical-design': [
        { role: 'architect', provider: 'anthropic', model: 'claude-opus-5-5', limits: undefined },
      ],
      implementation: [
        { role: 'coding', provider: 'anthropic', model: 'claude-sonnet-5-5', limits: undefined },
      ],
      verification: [
        { role: 'review', provider: 'openai', model: 'gpt-5.2', limits: undefined },
        reasoning,
      ],
      delivery: [{ role: 'fast', provider: 'anthropic', model: 'claude-haiku-4-5', limits: undefined }],
    };
    for (const [mode, chain] of Object.entries(expected)) {
      expect(routing.purposeChains.get(mode), mode).toEqual(chain);
    }
  });

  it('expands every catalog role and uses the first routing key as legacy fallback', () => {
    const routing = normalizeRouting(DEFAULT_CONFIG);
    expect([...routing.roleChains.keys()].sort()).toEqual([
      'architect',
      'coding',
      'fast',
      'reasoning',
      'review',
    ]);
    expect(routing.roleChains.get('review')).toEqual(routing.purposeChains.get('verification'));
    // First key of the default routing is `intent`.
    expect(routing.legacyFallback).toEqual(routing.purposeChains.get('intent'));
  });
});

describe('normalizing role references', () => {
  it('carries per-role limits through the chain', () => {
    const config = configWith(
      { ...INLINE_ROUTES, discovery: 'a' },
      {
        a: {
          provider: 'anthropic',
          model: 'claude-sonnet-5-5',
          fallbacks: ['b'],
          limits: { maxOutputTokens: 4096, temperature: 0.2 },
        },
        b: { provider: 'openai', model: 'gpt-5.2', limits: { maxOutputTokens: 2048 } },
      },
    );
    const routing = normalizeRouting(config);
    expect(routing.purposeChains.get('discovery')).toEqual([
      {
        role: 'a',
        provider: 'anthropic',
        model: 'claude-sonnet-5-5',
        limits: { maxOutputTokens: 4096, temperature: 0.2 },
      },
      {
        role: 'b',
        provider: 'openai',
        model: 'gpt-5.2',
        limits: { maxOutputTokens: 2048 },
      },
    ]);
    expect(routing.roleChains.get('a')).toEqual(routing.purposeChains.get('discovery'));
  });

  it('expands fallbacks breadth-first, deduplicating without reordering', () => {
    const config = configWith(
      { ...INLINE_ROUTES, discovery: 'a' },
      {
        a: { provider: 'fake', model: 'echo-a', fallbacks: ['b', 'c'] },
        b: { provider: 'fake', model: 'echo-b', fallbacks: ['c', 'd'] },
        c: { provider: 'fake', model: 'echo-c' },
        d: { provider: 'fake', model: 'echo-d' },
      },
    );
    expect(normalizeRouting(config).purposeChains.get('discovery')?.map((route) => route.role)).toEqual([
      'a',
      'b',
      'c',
      'd',
    ]);
  });

  it('hard-caps expanded chains at one primary plus MAX_FALLBACKS', () => {
    const config = configWith(
      { ...INLINE_ROUTES, discovery: 'a' },
      {
        a: { provider: 'fake', model: 'echo-a', fallbacks: ['b', 'c'] },
        b: { provider: 'fake', model: 'echo-b', fallbacks: ['d'] },
        c: { provider: 'fake', model: 'echo-c', fallbacks: ['e'] },
        d: { provider: 'fake', model: 'echo-d' },
        e: { provider: 'fake', model: 'echo-e' },
      },
    );
    // This catalog would fail effective validation; normalization still
    // behaves defensively and stops at four routes.
    expect(normalizeRouting(config).purposeChains.get('discovery')?.map((route) => route.role)).toEqual([
      'a',
      'b',
      'c',
      'd',
    ]);
  });

  it('resolves a role reference with no fallbacks to a single-route chain', () => {
    const config = configWith(
      { ...INLINE_ROUTES, verification: 'review' },
      { review: { provider: 'openai', model: 'gpt-5.2' } },
    );
    expect(normalizeRouting(config).purposeChains.get('verification')).toEqual([
      { role: 'review', provider: 'openai', model: 'gpt-5.2', limits: undefined },
    ]);
  });
});

describe('normalizing legacy inline routes', () => {
  it('marks inline routes and their fallbacks with role null, keeping limits', () => {
    const config = configWith({
      ...INLINE_ROUTES,
      discovery: {
        provider: 'openai',
        model: 'gpt-5.2',
        fallbacks: [{ provider: 'anthropic', model: 'claude-sonnet-5-5' }],
        limits: { temperature: 0.1 },
      },
    });
    expect(normalizeRouting(config).purposeChains.get('discovery')).toEqual([
      { role: null, provider: 'openai', model: 'gpt-5.2', limits: { temperature: 0.1 } },
      {
        role: null,
        provider: 'anthropic',
        model: 'claude-sonnet-5-5',
        limits: undefined,
      },
    ]);
  });

  it('produces an empty role catalog and the first route as legacy fallback', () => {
    const config = configWith(INLINE_ROUTES);
    const routing = normalizeRouting(config);
    expect(routing.roleChains.size).toBe(0);
    expect(routing.purposeChains.size).toBe(7);
    expect(routing.purposeChains.has('intent')).toBe(false);
    expect(routing.legacyFallback).toEqual([
      { role: null, provider: 'anthropic', model: 'claude-sonnet-5-5', limits: undefined },
    ]);
  });
});

describe('normalization robustness', () => {
  it('is deterministic and does not mutate the configuration', () => {
    const config = configWith(
      { ...INLINE_ROUTES, intent: 'a', discovery: 'a' },
      {
        a: { provider: 'fake', model: 'echo-a', fallbacks: ['b'] },
        b: { provider: 'fake', model: 'echo-b' },
      },
    );
    const before = JSON.stringify(config);
    const first = normalizeRouting(config);
    const second = normalizeRouting(config);
    expect(first).toEqual(second);
    expect(JSON.stringify(config)).toBe(before);
    expect(first.purposeChains.get('intent')?.map((route) => route.role)).toEqual(['a', 'b']);
  });

  it('yields an empty chain for references to unknown roles (validated elsewhere)', () => {
    const config = configWith(
      { ...INLINE_ROUTES, discovery: 'ghost' },
      { a: { provider: 'fake', model: 'echo-a' } },
    );
    expect(normalizeRouting(config).purposeChains.get('discovery')).toEqual([]);
  });
});
