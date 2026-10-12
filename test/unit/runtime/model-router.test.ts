import { describe, expect, it } from 'vitest';
import type { RelayConfig } from '../../../src/config/schema.js';
import { FakeProvider } from '../../../src/providers/fake.js';
import type { ModelProvider, ProviderName } from '../../../src/providers/provider.js';
import { ModelRouter, RouteUnavailableError } from '../../../src/runtime/model-router.js';

function configWithRouting(routing: RelayConfig['routing']): RelayConfig {
  return {
    version: 1,
    providers: { openai: { apiKeyEnv: 'OPENAI_API_KEY' }, anthropic: { apiKeyEnv: 'ANTHROPIC_API_KEY' } },
    routing,
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

function providers(map: Partial<Record<ProviderName, ModelProvider>>): ReadonlyMap<ProviderName, ModelProvider> {
  return new Map(Object.entries(map) as Array<[ProviderName, ModelProvider]>);
}

const ROUTING: RelayConfig['routing'] = {
  discovery: {
    provider: 'anthropic',
    model: 'claude-sonnet-5-5',
    fallbacks: [{ provider: 'openai', model: 'gpt-5.2' }],
    limits: { maxOutputTokens: 4096, temperature: 0.4 },
  },
  planning: { provider: 'openai', model: 'gpt-5-mini' },
  implementation: { provider: 'fake', model: 'fake-standard' },
};

describe('ModelRouter', () => {
  it('routes by mode with limits and provider resolution', () => {
    const fake = new FakeProvider([]);
    const router = new ModelRouter(
      providers({ anthropic: fake, openai: new FakeProvider([]) }),
      ROUTING,
    );
    const route = router.route({ mode: 'discovery', purpose: 'mode:discovery' });
    expect(route.providerName).toBe('anthropic');
    expect(route.model).toBe('claude-sonnet-5-5');
    expect(route.limits).toEqual({ maxOutputTokens: 4096, temperature: 0.4 });
    expect(route.provider).toBe(fake);
  });

  it('falls back to the first configured route for non-mode purposes', () => {
    const router = new ModelRouter(providers({ anthropic: new FakeProvider([]) }), ROUTING);
    const route = router.route({ purpose: 'intent' });
    expect(route.providerName).toBe('anthropic');
    expect(route.model).toBe('claude-sonnet-5-5');
    expect(router.route({ mode: null, purpose: 'status' }).model).toBe('claude-sonnet-5-5');
  });

  it('routeChain returns primary plus registered fallbacks in order', () => {
    const router = new ModelRouter(
      providers({ anthropic: new FakeProvider([]), openai: new FakeProvider([]) }),
      ROUTING,
    );
    const chain = router.routeChain({ mode: 'discovery', purpose: 'mode:discovery' });
    expect(chain.map((route) => [route.providerName, route.model])).toEqual([
      ['anthropic', 'claude-sonnet-5-5'],
      ['openai', 'gpt-5.2'],
    ]);
  });

  it('filters unregistered fallback providers out of the chain', () => {
    const router = new ModelRouter(providers({ anthropic: new FakeProvider([]) }), ROUTING);
    const chain = router.routeChain({ mode: 'discovery', purpose: 'mode:discovery' });
    expect(chain).toHaveLength(1);
    expect(chain[0]?.providerName).toBe('anthropic');
  });

  it('routes to the fake provider when configured', () => {
    const fake = new FakeProvider([]);
    const router = new ModelRouter(providers({ fake }), ROUTING);
    const route = router.route({ mode: 'implementation', purpose: 'mode:implementation' });
    expect(route.providerName).toBe('fake');
    expect(route.provider).toBe(fake);
    expect(route.limits).toBeUndefined();
  });

  it('throws RouteUnavailableError naming the provider and mode when unregistered', () => {
    const router = new ModelRouter(providers({ anthropic: new FakeProvider([]) }), ROUTING);
    expect(() => router.route({ mode: 'planning', purpose: 'mode:planning' })).toThrowError(
      RouteUnavailableError,
    );
    try {
      router.route({ mode: 'planning', purpose: 'mode:planning' });
    } catch (error) {
      const message = (error as Error).message;
      expect(message).toContain('"openai"');
      expect(message).toContain('"planning"');
      expect(message).toContain('Register the provider');
    }
  });

  it('throws RouteUnavailableError when no routing is configured at all', () => {
    const router = new ModelRouter(providers({ fake: new FakeProvider([]) }), {});
    expect(() => router.route({ purpose: 'intent' })).toThrowError(RouteUnavailableError);
    expect(() => router.route({ mode: 'discovery', purpose: 'mode:discovery' })).toThrowError(
      /no route is configured/,
    );
  });

  it('does not let mode routing shadow a missing entry with another mode', () => {
    const routing = configWithRouting({
      verification: { provider: 'fake', model: 'fake-standard' },
    }).routing;
    const router = new ModelRouter(providers({ fake: new FakeProvider([]) }), routing);
    // delivery is a real mode but unconfigured: falls back to first configured.
    const route = router.route({ mode: 'delivery', purpose: 'mode:delivery' });
    expect(route.providerName).toBe('fake');
  });
});
