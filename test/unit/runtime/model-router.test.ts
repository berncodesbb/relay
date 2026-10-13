import { describe, expect, it } from 'vitest';
import type { ModelRoleConfig, RelayConfig } from '../../../src/config/schema.js';
import { FakeProvider } from '../../../src/providers/fake.js';
import type { ModelProvider, ProviderName } from '../../../src/providers/provider.js';
import { ModelRouter, RouteUnavailableError } from '../../../src/runtime/model-router.js';

function configWithRouting(
  routing: RelayConfig['routing'],
  models?: Record<string, ModelRoleConfig>,
): RelayConfig {
  return {
    version: 1,
    providers: { openai: { apiKeyEnv: 'OPENAI_API_KEY' }, anthropic: { apiKeyEnv: 'ANTHROPIC_API_KEY' } },
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

function providers(map: Partial<Record<ProviderName, ModelProvider>>): ReadonlyMap<ProviderName, ModelProvider> {
  return new Map(Object.entries(map) as Array<[ProviderName, ModelProvider]>);
}

/** Legacy inline-only routing (no models catalog, no intent key). */
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

/** Role-catalog routing: modes through roles, intent explicitly on `fast`. */
function rolesConfig(): RelayConfig {
  return configWithRouting(
    {
      intent: 'fast',
      discovery: 'reasoning',
      planning: 'review',
      implementation: 'coding',
    },
    {
      reasoning: {
        provider: 'anthropic',
        model: 'claude-sonnet-5-5',
        limits: { maxOutputTokens: 4096, temperature: 0.4 },
      },
      review: { provider: 'openai', model: 'gpt-5.2', fallbacks: ['reasoning'] },
      coding: { provider: 'anthropic', model: 'claude-sonnet-5-5' },
      fast: { provider: 'anthropic', model: 'claude-haiku-4-5' },
    },
  );
}

describe('ModelRouter (legacy inline routing)', () => {
  it('routes by mode with limits and provider resolution', () => {
    const fake = new FakeProvider([]);
    const router = new ModelRouter(
      providers({ anthropic: fake, openai: new FakeProvider([]) }),
      configWithRouting(ROUTING),
    );
    const route = router.route({ mode: 'discovery', purpose: 'mode:discovery' });
    expect(route.providerName).toBe('anthropic');
    expect(route.model).toBe('claude-sonnet-5-5');
    expect(route.limits).toEqual({ maxOutputTokens: 4096, temperature: 0.4 });
    expect(route.provider).toBe(fake);
  });

  it('falls back to the first configured route for non-mode purposes when no intent entry exists', () => {
    const router = new ModelRouter(providers({ anthropic: new FakeProvider([]) }), configWithRouting(ROUTING));
    const route = router.route({ purpose: 'intent' });
    expect(route.providerName).toBe('anthropic');
    expect(route.model).toBe('claude-sonnet-5-5');
    expect(router.route({ mode: null, purpose: 'status' }).model).toBe('claude-sonnet-5-5');
  });

  it('routeChain returns primary plus registered fallbacks in order', () => {
    const router = new ModelRouter(
      providers({ anthropic: new FakeProvider([]), openai: new FakeProvider([]) }),
      configWithRouting(ROUTING),
    );
    const chain = router.routeChain({ mode: 'discovery', purpose: 'mode:discovery' });
    expect(chain.map((route) => [route.providerName, route.model])).toEqual([
      ['anthropic', 'claude-sonnet-5-5'],
      ['openai', 'gpt-5.2'],
    ]);
  });

  it('filters unregistered fallback providers out of the chain', () => {
    const router = new ModelRouter(providers({ anthropic: new FakeProvider([]) }), configWithRouting(ROUTING));
    const chain = router.routeChain({ mode: 'discovery', purpose: 'mode:discovery' });
    expect(chain).toHaveLength(1);
    expect(chain[0]?.providerName).toBe('anthropic');
  });

  it('routes to the fake provider when configured', () => {
    const fake = new FakeProvider([]);
    const router = new ModelRouter(providers({ fake }), configWithRouting(ROUTING));
    const route = router.route({ mode: 'implementation', purpose: 'mode:implementation' });
    expect(route.providerName).toBe('fake');
    expect(route.provider).toBe(fake);
    expect(route.limits).toBeUndefined();
  });

  it('reports role null for inline routes and never fabricates a role name', () => {
    const router = new ModelRouter(
      providers({ anthropic: new FakeProvider([]), openai: new FakeProvider([]) }),
      configWithRouting(ROUTING),
    );
    expect(router.route({ mode: 'discovery', purpose: 'mode:discovery' }).role).toBeNull();
    expect(router.route({ purpose: 'intent' }).role).toBeNull();
    const chain = router.routeChain({ mode: 'discovery', purpose: 'mode:discovery' });
    expect(chain.every((route) => route.role === null)).toBe(true);
  });

  it('throws RouteUnavailableError naming the provider and mode when unregistered', () => {
    const router = new ModelRouter(providers({ anthropic: new FakeProvider([]) }), configWithRouting(ROUTING));
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
    const router = new ModelRouter(providers({ fake: new FakeProvider([]) }), configWithRouting({}));
    expect(() => router.route({ purpose: 'intent' })).toThrowError(RouteUnavailableError);
    expect(() => router.route({ mode: 'discovery', purpose: 'mode:discovery' })).toThrowError(
      /no route is configured/,
    );
  });

  it('fails on an unconfigured workflow mode instead of borrowing another mode (defensive)', () => {
    const config = configWithRouting({
      verification: { provider: 'fake', model: 'fake-standard' },
    });
    const router = new ModelRouter(providers({ fake: new FakeProvider([]) }), config);
    // delivery is a real mode but unconfigured: effective validation makes
    // this impossible; the router must fail naming the mode, never substitute.
    expect(() => router.route({ mode: 'delivery', purpose: 'mode:delivery' })).toThrowError(
      RouteUnavailableError,
    );
    try {
      router.route({ mode: 'delivery', purpose: 'mode:delivery' });
    } catch (error) {
      expect((error as Error).message).toContain('"delivery"');
    }
  });
});

describe('ModelRouter (named model roles)', () => {
  it('resolves role-referenced routes with the role name, provider, model, and limits', () => {
    const fake = new FakeProvider([]);
    const router = new ModelRouter(
      providers({ anthropic: fake, openai: new FakeProvider([]) }),
      rolesConfig(),
    );
    const route = router.route({ mode: 'discovery', purpose: 'mode:discovery' });
    expect(route.role).toBe('reasoning');
    expect(route.providerName).toBe('anthropic');
    expect(route.model).toBe('claude-sonnet-5-5');
    expect(route.limits).toEqual({ maxOutputTokens: 4096, temperature: 0.4 });
    expect(route.provider).toBe(fake);
  });

  it('expands role fallback chains in order, each fallback keeping its own role and limits', () => {
    const router = new ModelRouter(
      providers({ anthropic: new FakeProvider([]), openai: new FakeProvider([]) }),
      rolesConfig(),
    );
    const chain = router.routeChain({ mode: 'planning', purpose: 'mode:planning' });
    expect(chain.map((route) => [route.role, route.providerName, route.model])).toEqual([
      ['review', 'openai', 'gpt-5.2'],
      ['reasoning', 'anthropic', 'claude-sonnet-5-5'],
    ]);
    expect(chain[1]?.limits).toEqual({ maxOutputTokens: 4096, temperature: 0.4 });
  });

  it('filters unregistered fallback providers out of role chains', () => {
    const router = new ModelRouter(providers({ openai: new FakeProvider([]) }), rolesConfig());
    const chain = router.routeChain({ mode: 'planning', purpose: 'mode:planning' });
    expect(chain.map((route) => route.role)).toEqual(['review']);
  });

  it('throws RouteUnavailableError naming provider, role, and mode when the role route provider is unregistered', () => {
    const router = new ModelRouter(providers({ anthropic: new FakeProvider([]) }), rolesConfig());
    try {
      router.route({ mode: 'planning', purpose: 'mode:planning' });
      expect.unreachable('route() should have thrown');
    } catch (error) {
      expect(error).toBeInstanceOf(RouteUnavailableError);
      const message = (error as Error).message;
      expect(message).toContain('"openai"');
      expect(message).toContain('"review"');
      expect(message).toContain('"planning"');
      expect(message).toContain('Register the provider');
    }
  });

  it('routes an explicit intent purpose entry before the mode and the legacy fallback', () => {
    const router = new ModelRouter(providers({ anthropic: new FakeProvider([]) }), rolesConfig());
    // With a mode present (the controller classifies intent in the current mode).
    expect(router.route({ mode: 'discovery', purpose: 'intent' })).toMatchObject({
      role: 'fast',
      model: 'claude-haiku-4-5',
    });
    // Without a mode.
    expect(router.route({ mode: null, purpose: 'intent' }).role).toBe('fast');
  });

  it('chainForRole returns the expanded chain for a known role', () => {
    const router = new ModelRouter(
      providers({ anthropic: new FakeProvider([]), openai: new FakeProvider([]) }),
      rolesConfig(),
    );
    expect(router.chainForRole('review').map((route) => route.role)).toEqual(['review', 'reasoning']);
    expect(router.chainForRole('reasoning').map((route) => route.model)).toEqual(['claude-sonnet-5-5']);
  });

  it('chainForRole throws for an unknown role and lists the available roles', () => {
    const router = new ModelRouter(providers({ anthropic: new FakeProvider([]) }), rolesConfig());
    try {
      router.chainForRole('nonexistent');
      expect.unreachable('chainForRole() should have thrown');
    } catch (error) {
      expect(error).toBeInstanceOf(RouteUnavailableError);
      const message = (error as Error).message;
      expect(message).toContain('"nonexistent"');
      expect(message).toContain('reasoning');
      expect(message).toContain('review');
      expect(message).toContain('fast');
    }
  });

  it('resolves deterministically across repeated calls', () => {
    const router = new ModelRouter(
      providers({ anthropic: new FakeProvider([]), openai: new FakeProvider([]) }),
      rolesConfig(),
    );
    const chain = router.routeChain({ mode: 'planning', purpose: 'mode:planning' });
    for (let call = 0; call < 3; call += 1) {
      expect(router.routeChain({ mode: 'planning', purpose: 'mode:planning' })).toEqual(chain);
    }
    expect(router.chainForRole('review')).toEqual(router.chainForRole('review'));
    expect(router.route({ purpose: 'intent' })).toEqual(router.route({ mode: null, purpose: 'intent' }));
  });
});
