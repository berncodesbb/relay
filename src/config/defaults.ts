/**
 * Default configuration.
 *
 * Every mode routes to a concrete provider and model. The fake provider is a
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
  routing: {
    discovery: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
    specification: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
    'technical-design': { provider: 'anthropic', model: 'claude-opus-5-5' },
    planning: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
    implementation: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
    verification: { provider: 'openai', model: 'gpt-5.2' },
    delivery: { provider: 'anthropic', model: 'claude-haiku-4-5' },
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
