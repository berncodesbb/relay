import { describe, expect, it } from 'vitest';
import { ToolRegistrationError, ToolRegistry, toolJsonSchema } from '../../../src/tools/registry.js';
import { createDefaultToolRegistry } from '../../../src/tools/index.js';
import { testSettings } from './test-kit.js';

const EXPECTED_TOOLS: readonly string[] = [
  'apply-patch',
  'git-diff',
  'git-log',
  'git-status',
  'list-dir',
  'read-file',
  'repo-instructions',
  'run-command',
  'search-text',
  'write-file',
];

describe('createDefaultToolRegistry', () => {
  it('registers every built-in tool exactly once', () => {
    const registry = createDefaultToolRegistry(testSettings());
    const names = registry.list().map((tool) => tool.name).sort();
    expect(names).toEqual([...EXPECTED_TOOLS].sort());
    for (const name of EXPECTED_TOOLS) {
      expect(registry.has(name)).toBe(true);
    }
  });

  it('classifies tools by capability', () => {
    const registry = createDefaultToolRegistry(testSettings());
    expect(registry.listByCapability('write').map((tool) => tool.name).sort()).toEqual(['apply-patch', 'write-file']);
    expect(registry.listByCapability('execute').map((tool) => tool.name)).toEqual(['run-command']);
    expect(registry.listByCapability('read').map((tool) => tool.name).sort()).toEqual(
      ['git-diff', 'git-log', 'git-status', 'list-dir', 'read-file', 'repo-instructions', 'search-text'],
    );
  });

  it('exposes provider tool specs with closed JSON Schema parameters', () => {
    const registry = createDefaultToolRegistry(testSettings());
    const specs = registry.toProviderTools();
    expect(specs.length).toBe(EXPECTED_TOOLS.length);
    for (const spec of specs) {
      const parameters = spec.parameters;
      expect(typeof spec.description).toBe('string');
      expect(spec.description.length).toBeGreaterThan(0);
      expect(parameters.type).toBe('object');
      expect(parameters.additionalProperties).toBe(false);
      expect(parameters.properties).toBeTypeOf('object');
    }
    const runCommand = specs.find((spec) => spec.name === 'run-command');
    expect(Object.keys(runCommand?.parameters.properties ?? {})).toEqual(
      expect.arrayContaining(['command', 'args', 'cwd', 'timeoutMs', 'env']),
    );
    const byCapability = registry.toProviderTools(['read']);
    expect(byCapability.length).toBe(7);
  });

  it('produces per-tool JSON Schemas for every registered tool', () => {
    const registry = createDefaultToolRegistry(testSettings());
    for (const tool of registry.list()) {
      const schema = toolJsonSchema(tool);
      expect(schema.additionalProperties).toBe(false);
      expect(schema.type).toBe('object');
    }
  });

  it('rejects duplicate registrations and unknown lookups', () => {
    const registry = createDefaultToolRegistry(testSettings());
    const existing = registry.get('read-file');
    expect(() => registry.register(existing)).toThrow(ToolRegistrationError);
    expect(() => registry.get('no-such-tool')).toThrow(/Unknown tool/);
    expect(() => {
      const fresh = new ToolRegistry();
      fresh.register({
        ...existing,
        name: 'BadName',
        execute: existing.execute,
      });
    }).toThrow(ToolRegistrationError);
  });

  it('gives every tool a positive timeout and output cap', () => {
    const registry = createDefaultToolRegistry(testSettings());
    for (const tool of registry.list()) {
      expect(tool.timeoutMs).toBeGreaterThan(0);
      expect(tool.maxOutputChars).toBeGreaterThan(0);
    }
  });
});
