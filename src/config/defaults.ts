/**
 * Default configuration.
 *
 * The seven workflow modes and the `intent` purpose route through named model
 * roles defined in the `models` catalog. The defaults resolve to exactly the
 * providers and models the mode-based defaults always used: Anthropic Sonnet
 * for the reasoning-heavy stages, Anthropic Opus for technical design, OpenAI
 * with a reasoning fallback for verification, and the lightweight Anthropic
 * Haiku role for delivery and intent classification. The fake provider is a
 * valid target for demos and deterministic tests; it never contacts the
 * network.
 */

import type { RelayConfig } from './schema.js';

export const DEFAULT_CONFIG: RelayConfig = {
  version: 1,
  providers: {
    openai: { apiKeyEnv: 'OPENAI_API_KEY' },
    anthropic: { apiKeyEnv: 'ANTHROPIC_API_KEY' },
  },
  models: {
    reasoning: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
    architect: { provider: 'anthropic', model: 'claude-opus-5-5' },
    coding: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
    review: { provider: 'openai', model: 'gpt-5.2', fallbacks: ['reasoning'] },
    fast: { provider: 'anthropic', model: 'claude-haiku-4-5' },
  },
  routing: {
    intent: 'fast',
    discovery: 'reasoning',
    specification: 'reasoning',
    'technical-design': 'architect',
    planning: 'reasoning',
    implementation: 'coding',
    verification: 'review',
    delivery: 'fast',
  },
  budgets: {
    maxTurns: 24,
    maxModelCalls: 48,
    maxToolCalls: 96,
    maxOutputTokens: 400_000,
    wallClockMs: 1_800_000,
    maxRepeatedActions: 3,
  },
  autonomy: 'next-checkpoint',
  tools: {
    commandTimeoutMs: 120_000,
    maxSearchResults: 200,
    maxFileBytes: 2_000_000,
  },
};
