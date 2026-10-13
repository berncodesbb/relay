/**
 * Model-role catalog and union routing schema tests.
 *
 * Covers the raw configuration surface introduced by the named model-role
 * work: the `models` catalog with user-defined role names, routing values
 * that are either a role-name string or a legacy inline object in the same
 * file, the canonical role-name rule, and the `intent` routing key. Layer
 * fixtures include `budgets: {}` because every parsed layer requires budgets.
 */

import { describe, expect, it } from 'vitest';
import { parse as parseYaml } from 'yaml';
import {
  ROLE_NAME_PATTERN,
  ConfigValidationError,
  modelRoleSchema,
  parseRelayConfig,
  roleNameSchema,
  routingValueSchema,
  type RelayConfig,
} from '../../../src/config/schema.js';

function parseYamlConfig(text: string): RelayConfig {
  return parseRelayConfig('roles.yaml', parseYaml(text) as object);
}

/** The plan's default models/routing block, verbatim, plus required headers. */
const PLAN_DEFAULT_YAML = `
version: 1
budgets: {}
models:
  reasoning:
    provider: anthropic
    model: claude-sonnet-5-5
  architect:
    provider: anthropic
    model: claude-opus-5-5
  coding:
    provider: anthropic
    model: claude-sonnet-5-5
  review:
    provider: openai
    model: gpt-5.2
    fallbacks: [reasoning]
  fast:
    provider: anthropic
    model: claude-haiku-4-5

routing:
  intent: fast
  discovery: reasoning
  specification: reasoning
  technical-design: architect
  planning: reasoning
  implementation: coding
  verification: review
  delivery: fast
`;

describe('model-role catalog parsing', () => {
  it('parses the plan default block verbatim', () => {
    const parsed = parseYamlConfig(PLAN_DEFAULT_YAML);
    expect(parsed.models?.reasoning).toEqual({ provider: 'anthropic', model: 'claude-sonnet-5-5' });
    expect(parsed.models?.review).toEqual({
      provider: 'openai',
      model: 'gpt-5.2',
      fallbacks: ['reasoning'],
    });
    expect(parsed.routing.intent).toBe('fast');
    expect(parsed.routing.verification).toBe('review');
  });

  it('accepts user-defined role names such as product, ux-writing, security-review', () => {
    const parsed = parseYamlConfig(`
version: 1
budgets: {}
models:
  product: { provider: openai, model: gpt-5.2 }
  ux-writing: { provider: anthropic, model: claude-haiku-4-5 }
  security-review: { provider: openai, model: gpt-5.2, fallbacks: [product] }
routing:
  discovery: product
  specification: product
  technical-design: product
  planning: product
  implementation: product
  verification: security-review
  delivery: ux-writing
`);
    expect(Object.keys(parsed.models ?? {}).sort()).toEqual([
      'product',
      'security-review',
      'ux-writing',
    ]);
    expect(parsed.models?.['security-review']?.fallbacks).toEqual(['product']);
  });

  it('parses per-role limits', () => {
    const parsed = parseYamlConfig(`
version: 1
budgets: {}
models:
  reasoning:
    provider: anthropic
    model: claude-sonnet-5-5
    limits: { maxOutputTokens: 8192, temperature: 0.3 }
routing:
  discovery: reasoning
  specification: reasoning
  technical-design: reasoning
  planning: reasoning
  implementation: reasoning
  verification: reasoning
  delivery: reasoning
`);
    expect(parsed.models?.reasoning?.limits).toEqual({ maxOutputTokens: 8192, temperature: 0.3 });
    expect(modelRoleSchema.parse(parsed.models?.reasoning).limits?.temperature).toBe(0.3);
  });

  it('rejects invalid role names in the catalog with actionable messages', () => {
    const badNames = ['', 'Reasoning', 'a'.repeat(64) + 'b', 'a/b', 'a_b'];
    for (const name of badNames) {
      const yamlKey = name === '' ? '""' : JSON.stringify(name);
      expect(
        () =>
          parseYamlConfig(`
version: 1
budgets: {}
models:
  ${yamlKey}: { provider: anthropic, model: claude-sonnet-5-5 }
routing:
  discovery: { provider: anthropic, model: claude-sonnet-5-5 }
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: { provider: anthropic, model: claude-opus-5-5 }
  planning: { provider: anthropic, model: claude-sonnet-5-5 }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
`),
        `role name ${JSON.stringify(name)}`,
      ).toThrow(ConfigValidationError);
      expect(() => roleNameSchema.parse(name), `role name ${JSON.stringify(name)}`).toThrow(
        /kebab-case/,
      );
    }
  });

  it('exposes one canonical role-name pattern', () => {
    expect(ROLE_NAME_PATTERN.test('reasoning')).toBe(true);
    expect(ROLE_NAME_PATTERN.test('ux-writing')).toBe(true);
    expect(ROLE_NAME_PATTERN.test('a')).toBe(true);
    expect(ROLE_NAME_PATTERN.test('a'.repeat(64))).toBe(true);
    expect(ROLE_NAME_PATTERN.test('')).toBe(false);
    expect(ROLE_NAME_PATTERN.test('Reasoning')).toBe(false);
    expect(ROLE_NAME_PATTERN.test('a'.repeat(65))).toBe(false);
    expect(ROLE_NAME_PATTERN.test('a/b')).toBe(false);
    expect(ROLE_NAME_PATTERN.test('a_b')).toBe(false);
  });

  it('validates fallback entries as role names and caps their count', () => {
    expect(() =>
      modelRoleSchema.parse({ provider: 'openai', model: 'gpt-5.2', fallbacks: ['Reasoning'] }),
    ).toThrow(/kebab-case/);
    expect(() =>
      modelRoleSchema.parse({
        provider: 'openai',
        model: 'gpt-5.2',
        fallbacks: ['a', 'b', 'c', 'd'],
      }),
    ).toThrow(/fallbacks/);
  });
});

describe('union routing values', () => {
  it('parses a file mixing a role-name intent with inline mode objects', () => {
    const parsed = parseYamlConfig(`
version: 1
budgets: {}
models:
  fast: { provider: anthropic, model: claude-haiku-4-5 }
routing:
  intent: fast
  discovery: { provider: anthropic, model: claude-sonnet-5-5 }
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: { provider: anthropic, model: claude-opus-5-5, fallbacks: [{ provider: openai, model: gpt-5.2 }] }
  planning: { provider: anthropic, model: claude-sonnet-5-5 }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
`);
    expect(parsed.routing.intent).toBe('fast');
    expect(parsed.routing.discovery).toEqual({ provider: 'anthropic', model: 'claude-sonnet-5-5' });
    expect(parsed.routing['technical-design']).toEqual({
      provider: 'anthropic',
      model: 'claude-opus-5-5',
      fallbacks: [{ provider: 'openai', model: 'gpt-5.2' }],
    });
  });

  it('accepts the intent key alongside the seven workflow modes', () => {
    const parsed = parseYamlConfig(PLAN_DEFAULT_YAML);
    expect(Object.keys(parsed.routing).sort()).toEqual([
      'delivery',
      'discovery',
      'implementation',
      'intent',
      'planning',
      'specification',
      'technical-design',
      'verification',
    ]);
  });

  it('rejects routing keys that are neither modes nor intent, naming the key', () => {
    expect(() =>
      parseYamlConfig(`
version: 1
budgets: {}
routing:
  intent: { provider: anthropic, model: claude-haiku-4-5 }
  frobnicate: { provider: openai, model: gpt-5.2 }
  discovery: { provider: anthropic, model: claude-sonnet-5-5 }
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: { provider: anthropic, model: claude-opus-5-5 }
  planning: { provider: anthropic, model: claude-sonnet-5-5 }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
`),
    ).toThrow(/unknown modes.*frobnicate|frobnicate.*unknown/i);
  });

  it('still requires all seven workflow modes with role-based routing', () => {
    expect(() =>
      parseYamlConfig(`
version: 1
budgets: {}
models:
  reasoning: { provider: anthropic, model: claude-sonnet-5-5 }
routing:
  discovery: reasoning
  specification: reasoning
  technical-design: reasoning
  planning: reasoning
  implementation: reasoning
  verification: reasoning
`),
    ).toThrow(/missing modes.*delivery/i);
  });

  it('rejects routing values that are neither role names nor inline routes', () => {
    expect(() =>
      parseYamlConfig(`
version: 1
budgets: {}
routing:
  discovery: 42
  specification: { provider: anthropic, model: claude-sonnet-5-5 }
  technical-design: { provider: anthropic, model: claude-opus-5-5 }
  planning: { provider: anthropic, model: claude-sonnet-5-5 }
  implementation: { provider: anthropic, model: claude-sonnet-5-5 }
  verification: { provider: openai, model: gpt-5.2 }
  delivery: { provider: anthropic, model: claude-haiku-4-5 }
`),
    ).toThrow(/role name|inline route/i);

    expect(routingValueSchema.parse('reasoning')).toBe('reasoning');
    expect(routingValueSchema.parse({ provider: 'openai', model: 'gpt-5.2' })).toEqual({
      provider: 'openai',
      model: 'gpt-5.2',
    });
    expect(routingValueSchema.safeParse(42).success).toBe(false);
  });

  it('keeps schema version at 1 for the backward-compatible additions', () => {
    const parsed = parseYamlConfig(PLAN_DEFAULT_YAML);
    expect(parsed.version).toBe(1);
  });
});
