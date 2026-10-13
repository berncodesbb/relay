/**
 * Layered loading, effective-configuration validation, and overrides tests
 * for the named model-role work.
 *
 * Covers: role-catalog merge by name across layers (later layers replace one
 * complete role, unrelated roles survive), routing merge by key including
 * `intent`, cross-reference validation of the merged configuration (unknown
 * role references, unknown fallback roles, self-reference, cycles, excessive
 * expanded chains), source-aware error prefixes, forceProvider over both the
 * role catalog and inline routes, and dotted `relay config get` lookups.
 */

import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';
import { DEFAULT_CONFIG } from '../../../src/config/defaults.js';
import {
  type EffectiveConfigOrigins,
  loadConfig,
  validateEffectiveConfig,
} from '../../../src/config/load.js';
import {
  ConfigValidationError,
  type ModelRoleConfig,
  type RelayConfig,
  type RoutingTable,
} from '../../../src/config/schema.js';

const dirs: string[] = [];

afterEach(() => {
  while (dirs.length > 0) rmSync(dirs.pop() as string, { recursive: true, force: true });
});

function workspaceWithProjectConfig(yaml: string | undefined): string {
  const dir = mkdtempSync(join(tmpdir(), 'relay-cfg-load-'));
  dirs.push(dir);
  if (yaml !== undefined) writeFileSync(join(dir, 'relay.config.yaml'), yaml);
  return dir;
}

function writeUserConfig(yaml: string): string {
  const dir = mkdtempSync(join(tmpdir(), 'relay-cfg-user-'));
  dirs.push(dir);
  const path = join(dir, 'user.yaml');
  writeFileSync(path, yaml);
  return path;
}

/** A role-based layer routing all seven modes (per-layer requirement). */
const ROLE_ROUTING = `
routing:
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
`;

const USER_LAYER_YAML = `
version: 1
budgets: {}
models:
  reasoning: { provider: openai, model: gpt-5.2 }
${ROLE_ROUTING}`;

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

function catchValidation(action: () => unknown): ConfigValidationError {
  try {
    action();
  } catch (error) {
    if (error instanceof ConfigValidationError) return error;
    throw error;
  }
  throw new Error('expected loadConfig to reject the configuration');
}

describe('role catalog layering', () => {
  it('defaults define the five default roles and route every mode plus intent', () => {
    const { config, layers } = loadConfig({ workspaceRoot: workspaceWithProjectConfig(undefined) });
    expect(layers.map((layer) => layer.source)).toEqual(['defaults']);
    expect(Object.keys(config.models ?? {}).sort()).toEqual([
      'architect',
      'coding',
      'fast',
      'reasoning',
      'review',
    ]);
    expect(config.routing.intent).toBe('fast');
    expect(config.routing.verification).toBe('review');
  });

  it('a user layer replaces one complete role while other roles and intent survive', () => {
    const workspace = workspaceWithProjectConfig(undefined);
    const { config } = loadConfig({
      workspaceRoot: workspace,
      userConfigPath: writeUserConfig(USER_LAYER_YAML),
    });
    expect(config.models?.reasoning).toEqual({ provider: 'openai', model: 'gpt-5.2' });
    expect(config.models?.fast).toEqual(DEFAULT_CONFIG.models?.fast);
    expect(config.models?.review).toEqual({
      provider: 'openai',
      model: 'gpt-5.2',
      fallbacks: ['reasoning'],
    });
    // The user layer did not define intent: the default entry survives.
    expect(config.routing.intent).toBe('fast');
  });

  it('a project layer overrides the user layer and unrelated user roles survive (AC-4)', () => {
    const workspace = workspaceWithProjectConfig(`
version: 1
budgets: {}
models:
  reasoning: { provider: fake, model: echo-1 }
${ROLE_ROUTING}`);
    const { config } = loadConfig({
      workspaceRoot: workspace,
      userConfigPath: writeUserConfig(`
version: 1
budgets: {}
models:
  reasoning: { provider: openai, model: gpt-5.2 }
  product: { provider: fake, model: demo-product }
routing:
  intent: product
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast`),
    });
    expect(config.models?.reasoning).toEqual({ provider: 'fake', model: 'echo-1' });
    expect(config.models?.product).toEqual({ provider: 'fake', model: 'demo-product' });
    expect(config.models?.fast).toEqual(DEFAULT_CONFIG.models?.fast);
    // Routing merges by key: user's intent survives the project layer.
    expect(config.routing.intent).toBe('product');
    expect(config.routing.discovery).toBe('reasoning');
  });

  it('routing merges by key, mixing role references and inline objects', () => {
    const workspace = workspaceWithProjectConfig(`
version: 1
budgets: {}
routing:
  intent: reasoning
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: { provider: fake, model: echo-v }
  delivery: fast
`);
    const { config } = loadConfig({ workspaceRoot: workspace });
    expect(config.routing.intent).toBe('reasoning');
    expect(config.routing.verification).toEqual({ provider: 'fake', model: 'echo-v' });
    expect(config.routing.delivery).toBe('fast');
  });

  it('a legacy layer without models leaves the merged catalog untouched but defined', () => {
    const workspace = workspaceWithProjectConfig(`
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
`);
    const { config } = loadConfig({ workspaceRoot: workspace });
    expect(config.models).toEqual(DEFAULT_CONFIG.models);
    expect(config.routing.verification).toEqual({ provider: 'openai', model: 'gpt-5.2' });
    // The default intent entry still routes through the role catalog.
    expect(config.routing.intent).toBe('fast');
  });
});

describe('forceProvider with model roles', () => {
  it('rewrites providers across the catalog and inline routes, keeping everything else', () => {
    const workspace = workspaceWithProjectConfig(`
version: 1
budgets: {}
models:
  reasoning:
    provider: openai
    model: gpt-5.2
    limits: { maxOutputTokens: 4096 }
routing:
  intent: fast
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: { provider: anthropic, model: claude-sonnet-5-5, fallbacks: [{ provider: openai, model: gpt-5.2 }] }
  implementation: coding
  verification: review
  delivery: fast
`);
    const { config } = loadConfig({
      workspaceRoot: workspace,
      overrides: { forceProvider: 'fake' },
    });

    for (const [role, definition] of Object.entries(config.models ?? {})) {
      expect(definition.provider, role).toBe('fake');
    }
    expect(config.models?.reasoning).toEqual({
      provider: 'fake',
      model: 'gpt-5.2',
      limits: { maxOutputTokens: 4096 },
    });
    // Role-name fallback references are untouched.
    expect(config.models?.review?.fallbacks).toEqual(['reasoning']);
    expect(config.models?.fast?.model).toBe('claude-haiku-4-5');
    // Role-reference routing values stay role references.
    expect(config.routing.intent).toBe('fast');
    expect(config.routing.discovery).toBe('reasoning');
    // Inline routes and inline fallback objects are rewritten, models kept.
    expect(config.routing.planning).toEqual({
      provider: 'fake',
      model: 'claude-sonnet-5-5',
      fallbacks: [{ provider: 'fake', model: 'gpt-5.2' }],
    });
  });
});

describe('effective configuration validation', () => {
  it('rejects a routing reference to an unknown role with a dotted path and available roles', () => {
    const workspace = workspaceWithProjectConfig(`
version: 1
budgets: {}
routing:
  discovery: ghost
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
`);
    const error = catchValidation(() => loadConfig({ workspaceRoot: workspace }));
    expect(error.source).toBe(join(workspace, 'relay.config.yaml'));
    expect(error.message).toMatch(/routing\.discovery/);
    expect(error.message).toMatch(/"ghost"/);
    expect(error.message).toMatch(/available roles: reasoning, architect, coding, review, fast/);
  });

  it('rejects an unknown fallback role with the fallback index', () => {
    const workspace = workspaceWithProjectConfig(`
version: 1
budgets: {}
models:
  review:
    provider: openai
    model: gpt-5.2
    fallbacks: [ghost]
${ROLE_ROUTING}`);
    const error = catchValidation(() => loadConfig({ workspaceRoot: workspace }));
    expect(error.message).toMatch(/models\.review\.fallbacks\[0\]/);
    expect(error.message).toMatch(/"ghost"/);
  });

  it('rejects self-referencing roles', () => {
    const workspace = workspaceWithProjectConfig(`
version: 1
budgets: {}
models:
  review:
    provider: openai
    model: gpt-5.2
    fallbacks: [review]
${ROLE_ROUTING}`);
    const error = catchValidation(() => loadConfig({ workspaceRoot: workspace }));
    expect(error.message).toMatch(/models\.review\.fallbacks\[0\]/);
    expect(error.message).toMatch(/cannot fall back to itself/);
  });

  it('rejects cycles in the role fallback graph', () => {
    const workspace = workspaceWithProjectConfig(`
version: 1
budgets: {}
models:
  a: { provider: fake, model: echo-a, fallbacks: [b] }
  b: { provider: fake, model: echo-b, fallbacks: [a] }
routing:
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: a
  delivery: fast
`);
    const error = catchValidation(() => loadConfig({ workspaceRoot: workspace }));
    expect(error.message).toMatch(/cycle/);
    expect(error.message).toMatch(/a -> b -> a/);
  });

  it('rejects expanded chains longer than one primary plus three fallbacks', () => {
    const workspace = workspaceWithProjectConfig(`
version: 1
budgets: {}
models:
  a: { provider: fake, model: echo-a, fallbacks: [b, c] }
  b: { provider: fake, model: echo-b, fallbacks: [d, e] }
  c: { provider: fake, model: echo-c }
  d: { provider: fake, model: echo-d }
  e: { provider: fake, model: echo-e }
routing:
  discovery: a
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
`);
    const error = catchValidation(() => loadConfig({ workspaceRoot: workspace }));
    expect(error.message).toMatch(/routing\.discovery/);
    expect(error.message).toMatch(/"a" expands to 5/);
    expect(error.message).toMatch(/a -> b -> c -> d -> e/);
    expect(error.message).toMatch(/1 primary plus 3 fallbacks/);
  });

  it('rejects excessive chains on roles that routing never references (reachable only via --role)', () => {
    const workspace = workspaceWithProjectConfig(`
version: 1
budgets: {}
models:
  deep: { provider: fake, model: echo-deep, fallbacks: [d1, d2] }
  d1: { provider: fake, model: echo-d1, fallbacks: [d3, d4] }
  d2: { provider: fake, model: echo-d2 }
  d3: { provider: fake, model: echo-d3 }
  d4: { provider: fake, model: echo-d4 }
${ROLE_ROUTING}`);
    const error = catchValidation(() => loadConfig({ workspaceRoot: workspace }));
    expect(error.message).toMatch(/models\.deep/);
    expect(error.message).toMatch(/"deep" expands to 5/);
    expect(error.message).toMatch(/1 primary plus 3 fallbacks/);
  });

  it('accepts a valid four-deep expanded chain', () => {
    const workspace = workspaceWithProjectConfig(`
version: 1
budgets: {}
models:
  a: { provider: fake, model: echo-a, fallbacks: [b, c] }
  b: { provider: fake, model: echo-b, fallbacks: [d] }
  c: { provider: fake, model: echo-c }
  d: { provider: fake, model: echo-d }
routing:
  discovery: a
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
`);
    const { config } = loadConfig({ workspaceRoot: workspace });
    expect(config.routing.discovery).toBe('a');
  });

  it('attributes errors to the effective configuration when no layer file is known', () => {
    const config = configWith({
      discovery: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
      specification: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
      'technical-design': { provider: 'anthropic', model: 'claude-opus-5-5' },
      planning: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
      implementation: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
      verification: 'review',
      delivery: { provider: 'anthropic', model: 'claude-haiku-4-5' },
    });
    const error = catchValidation(() =>
      validateEffectiveConfig(config, 'the effective configuration'),
    );
    expect(error.source).toBe('the effective configuration');
    expect(error.message).toMatch(/routing\.verification: references unknown model role "review"/);
  });

  it('prefers the originating layer file when origins are provided', () => {
    const config = configWith({
      discovery: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
      specification: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
      'technical-design': { provider: 'anthropic', model: 'claude-opus-5-5' },
      planning: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
      implementation: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
      verification: 'ghost',
      delivery: { provider: 'anthropic', model: 'claude-haiku-4-5' },
    });
    const origins: EffectiveConfigOrigins = {
      routing: new Map([['verification', '/layers/project.yaml']]),
      models: new Map(),
    };
    const error = catchValidation(() =>
      validateEffectiveConfig(config, 'the effective configuration', origins),
    );
    expect(error.source).toBe('/layers/project.yaml');
  });
});

describe('dotted configuration lookups', () => {
  /** Mirrors the `relay config get <key>` dotted traversal. */
  function lookupDotted(source: unknown, key: string): { found: boolean; value: unknown } {
    let current: unknown = source;
    for (const part of key.split('.').filter((piece) => piece.length > 0)) {
      if (typeof current !== 'object' || current === null) return { found: false, value: undefined };
      const record = current as Record<string, unknown>;
      if (!(part in record)) return { found: false, value: undefined };
      current = record[part];
    }
    return { found: true, value: current };
  }

  it('resolves models.<role> and routing.intent on the effective object', () => {
    const { config } = loadConfig({ workspaceRoot: workspaceWithProjectConfig(undefined) });
    expect(lookupDotted(config, 'models.fast.model')).toEqual({
      found: true,
      value: 'claude-haiku-4-5',
    });
    expect(lookupDotted(config, 'models.review.fallbacks.0')).toEqual({
      found: true,
      value: 'reasoning',
    });
    expect(lookupDotted(config, 'routing.intent')).toEqual({ found: true, value: 'fast' });
    expect(lookupDotted(config, 'models.ghost')).toEqual({ found: false, value: undefined });
  });
});
