/**
 * Legacy configuration compatibility locks.
 *
 * These cases capture TODAY's behavior for complete version-1 configurations
 * that use only inline mode routes (no `models` catalog, no role references,
 * no `intent` key). The named-role routing work must keep every one of them
 * passing unchanged: an existing user/project config loads and behaves
 * exactly as before.
 */

import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { DEFAULT_CONFIG } from '../../../src/config/defaults.js';
import { loadConfig } from '../../../src/config/load.js';
import { parse as parseYaml } from 'yaml';
import { parseRelayConfig } from '../../../src/config/schema.js';
import { WORKFLOW_MODES } from '../../../src/core/modes.js';
import { FakeProvider } from '../../../src/providers/fake.js';
import type { ProviderName } from '../../../src/providers/provider.js';
import { ModelRouter } from '../../../src/runtime/model-router.js';
import type { RelayConfig } from '../../../src/config/schema.js';

// Note: today's schema requires `budgets` in every layer (it may be empty),
// so any real version-1 configuration includes it.
const LEGACY_YAML = `
version: 1
budgets: {}
routing:
  discovery: { provider: anthropic, model: claude-sonnet-5-5 }
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: { provider: anthropic, model: claude-opus-5-5 }
  planning: { provider: openai, model: gpt-5-mini, fallbacks: [{ provider: anthropic, model: claude-sonnet-5-5 }] }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
`;

function parseLegacy(text: string): RelayConfig {
  return parseRelayConfig('legacy.yaml', parseYaml(text) as object);
}

function providers(names: readonly ProviderName[]): ReadonlyMap<ProviderName, FakeProvider> {
  return new Map(names.map((name) => [name, new FakeProvider([])] as const));
}

describe('legacy inline routing compatibility', () => {
  it('parses a complete version-1 inline-only configuration unchanged', () => {
    const parsed = parseLegacy(LEGACY_YAML);
    expect(Object.keys(parsed.routing).sort()).toEqual([...WORKFLOW_MODES].sort());
    expect(parsed.routing.verification).toEqual({ provider: 'openai', model: 'gpt-5.2' });
    expect(parsed.routing.planning?.fallbacks).toEqual([
      { provider: 'anthropic', model: 'claude-sonnet-5-5' },
    ]);
  });

  it('resolves every workflow mode through inline routes without a models catalog', () => {
    const config = parseLegacy(LEGACY_YAML);
    const router = new ModelRouter(providers(['openai', 'anthropic']), config);
    expect(router.route({ mode: 'discovery', purpose: 'mode:discovery' }).model).toBe('claude-sonnet-5-5');
    expect(router.route({ mode: 'technical-design', purpose: 'mode:technical-design' }).model).toBe('claude-opus-5-5');
    expect(router.route({ mode: 'verification', purpose: 'mode:verification' }).providerName).toBe('openai');
    expect(router.route({ mode: 'delivery', purpose: 'mode:delivery' }).model).toBe('claude-haiku-4-5');
  });

  it('keeps the historical intent fallback: first configured route, insertion order', () => {
    const config = parseLegacy(LEGACY_YAML);
    const router = new ModelRouter(providers(['openai', 'anthropic']), config);
    const route = router.route({ purpose: 'intent' });
    expect(route.providerName).toBe('anthropic');
    expect(route.model).toBe('claude-sonnet-5-5');
  });

  it('preserves inline fallback order and filters unregistered providers', () => {
    const config = parseLegacy(LEGACY_YAML);
    // Only openai registered: the anthropic fallback drops out of the chain.
    const router = new ModelRouter(providers(['openai']), config);
    const chain = router.routeChain({ mode: 'planning', purpose: 'mode:planning' });
    expect(chain.map((route) => route.providerName)).toEqual(['openai']);
    const both = new ModelRouter(providers(['openai', 'anthropic']), config);
    expect(
      both.routeChain({ mode: 'planning', purpose: 'mode:planning' }).map((route) => route.model),
    ).toEqual(['gpt-5-mini', 'claude-sonnet-5-5']);
  });

  it('built-in defaults still route every mode to today\'s providers and models', () => {
    expect(DEFAULT_CONFIG.version).toBe(1);
    const router = new ModelRouter(providers(['openai', 'anthropic']), DEFAULT_CONFIG);
    const expected: Record<string, [ProviderName, string]> = {
      discovery: ['anthropic', 'claude-sonnet-5-5'],
      specification: ['anthropic', 'claude-sonnet-5-5'],
      'technical-design': ['anthropic', 'claude-opus-5-5'],
      planning: ['anthropic', 'claude-sonnet-5-5'],
      implementation: ['anthropic', 'claude-sonnet-5-5'],
      verification: ['openai', 'gpt-5.2'],
      delivery: ['anthropic', 'claude-haiku-4-5'],
    };
    for (const [mode, [providerName, model]] of Object.entries(expected)) {
      const route = router.route({ mode: mode as never, purpose: `mode:${mode}` });
      expect(route.providerName, mode).toBe(providerName);
      expect(route.model, mode).toBe(model);
    }
  });
});

describe('legacy layered loading compatibility', () => {
  const dirs: string[] = [];

  afterEach(() => {
    while (dirs.length > 0) rmSync(dirs.pop() as string, { recursive: true, force: true });
  });

  function workspaceWithProjectConfig(yaml: string): string {
    const dir = mkdtempSync(join(tmpdir(), 'relay-legacy-cfg-'));
    dirs.push(dir);
    writeFileSync(join(dir, 'relay.config.yaml'), yaml);
    return dir;
  }

  it('applies a legacy project layer over defaults without edits', () => {
    const workspace = workspaceWithProjectConfig(`
version: 1
budgets: {}
routing:
  discovery: { provider: openai, model: gpt-5-mini }
  specification: { provider: openai, model: gpt-5-mini }
  technical-design: { provider: openai, model: gpt-5.2 }
  planning: { provider: openai, model: gpt-5-mini }
  implementation: { provider: openai, model: gpt-5-mini }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: openai, model: gpt-5-mini }
`);
    const { config, layers } = loadConfig({ workspaceRoot: workspace });
    expect(layers.map((layer) => layer.source)).toContain('project');
    const router = new ModelRouter(providers(['openai']), config);
    expect(router.route({ mode: 'discovery', purpose: 'mode:discovery' }).model).toBe('gpt-5-mini');
    // Untouched layer values survive: budgets keep defaults.
    expect(config.budgets.maxTurns).toBe(DEFAULT_CONFIG.budgets.maxTurns);
  });

  it('forceProvider rewrites providers while keeping model identifiers', () => {
    const workspace = workspaceWithProjectConfig(LEGACY_YAML);
    const { config } = loadConfig({ workspaceRoot: workspace, overrides: { forceProvider: 'fake' } });
    const router = new ModelRouter(providers(['fake']), config);
    for (const mode of WORKFLOW_MODES) {
      const route = router.route({ mode, purpose: `mode:${mode}` });
      expect(route.providerName, mode).toBe('fake');
    }
    expect(router.route({ mode: 'verification', purpose: 'mode:verification' }).model).toBe('gpt-5.2');
  });

  it('rejects legacy routing keys that are not workflow modes', () => {
    expect(() =>
      parseLegacy(`
version: 1
budgets: {}
routing:
  discovery: { provider: anthropic, model: claude-sonnet-5-5 }
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: { provider: anthropic, model: claude-opus-5-5 }
  planning: { provider: anthropic, model: claude-sonnet-5-5 }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
  frobnicate: { provider: openai, model: gpt-5.2 }
`),
    ).toThrow(/unknown modes.*frobnicate|frobnicate.*unknown/i);
  });

  it('still requires all seven workflow modes', () => {
    expect(() => parseLegacy(`
version: 1
budgets: {}
routing:
  discovery: { provider: anthropic, model: claude-sonnet-5-5 }
`),
    ).toThrow(/missing modes/i);
  });

  it('RelayConfig type accepts the legacy full shape (compile-time lock)', () => {
    const config: RelayConfig = DEFAULT_CONFIG;
    expect(Object.keys(config.routing).length).toBeGreaterThanOrEqual(WORKFLOW_MODES.length);
  });
});
