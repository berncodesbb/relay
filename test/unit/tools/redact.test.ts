import { describe, expect, it } from 'vitest';
import { boundOutput, redactSecrets, sanitizeEnvForRecord } from '../../../src/tools/redact.js';

describe('redactSecrets', () => {
  it('redacts the literal value of secret-named environment variables', () => {
    process.env.RELAY_TEST_SECRET = 'hunter2-super-secret-value';
    try {
      const output = redactSecrets('password=hunter2-super-secret-value and more text');
      expect(output).toContain('[redacted:env]');
      expect(output).not.toContain('hunter2-super-secret-value');
    } finally {
      delete process.env.RELAY_TEST_SECRET;
    }
  });

  it('redacts values containing regular-expression metacharacters', () => {
    process.env.RELAY_TEST_SECRET = 'a.b*c(d)e';
    try {
      const output = redactSecrets('token a.b*c(d)e here');
      expect(output).toBe('token [redacted:env] here');
    } finally {
      delete process.env.RELAY_TEST_SECRET;
    }
  });

  it('ignores empty environment variable values', () => {
    process.env.RELAY_TEST_SECRET = '';
    try {
      expect(redactSecrets('unchanged text')).toBe('unchanged text');
    } finally {
      delete process.env.RELAY_TEST_SECRET;
    }
  });

  it('redacts OpenAI-style keys', () => {
    const output = redactSecrets('key: sk-proj-abcdefghijklmnop1234567890');
    expect(output).toBe('key: [redacted:api-key]');
  });

  it('redacts Anthropic-style keys', () => {
    const output = redactSecrets('key: sk-ant-api03-abcdefghijklmnop1234567890');
    expect(output).toBe('key: [redacted:anthropic-key]');
  });

  it('redacts AWS access keys', () => {
    const output = redactSecrets('aws AKIAIOSFODNN7EXAMPLE');
    expect(output).toBe('aws [redacted:aws-access-key]');
  });

  it('redacts GitHub and Slack tokens', () => {
    expect(redactSecrets('gh ghp_abcdefghijklmnopqrstuvwxyz123456')).toBe('gh [redacted:github-token]');
    expect(redactSecrets('slack xoxb-1234567890abcdef')).toBe('slack [redacted:slack-token]');
  });

  it('redacts bearer tokens', () => {
    const output = redactSecrets('Authorization: Bearer abcdefghijklmnopqrstuvwxyz123456');
    expect(output).toBe('Authorization: [redacted:bearer-token]');
    expect(output).not.toContain('abcdefghijklmnopqrstuvwxyz123456');
  });

  it('redacts private key blocks including their body', () => {
    const pem = '-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAabcdefghijklmnop\n-----END RSA PRIVATE KEY-----';
    const output = redactSecrets(`cert:\n${pem}\n`);
    expect(output).toBe('cert:\n[redacted:private-key]\n');
    expect(output).not.toContain('MIIEowIBAAKCAQEAabcdefghijklmnop');
  });

  it('leaves clean text untouched', () => {
    const text = 'const x = 1; console.log("task-manager"); // node:fs';
    expect(redactSecrets(text)).toBe(text);
  });
});

describe('boundOutput', () => {
  it('returns short text unchanged', () => {
    expect(boundOutput('hello', 100)).toBe('hello');
  });

  it('truncates long text with a marker that reports dropped characters', () => {
    const text = 'a'.repeat(500);
    const output = boundOutput(text, 100);
    expect(output.length).toBe(100);
    const match = output.match(/\n…\[truncated (\d+) chars\]/);
    expect(match).not.toBeNull();
    const kept = output.length - (match?.[0]?.length ?? 0);
    expect(text.startsWith(output.slice(0, kept))).toBe(true);
  });

  it('never exceeds the budget', () => {
    for (const max of [1, 5, 17, 64, 1000]) {
      expect(boundOutput('x'.repeat(10_000), max).length).toBeLessThanOrEqual(max);
    }
  });
});

describe('sanitizeEnvForRecord', () => {
  it('reduces entries to bare, deduplicated names', () => {
    expect(sanitizeEnvForRecord(['FOO', 'BAR=1', ' BAZ ', '', 'FOO=2'])).toEqual(['FOO', 'BAR', 'BAZ']);
  });
});
