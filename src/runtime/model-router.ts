/**
 * Fixed role- and purpose-aware routing with fallback chains.
 *
 * Routing is configuration, not model output: each workflow mode or explicit
 * non-mode purpose (today only `intent`) maps to a chain of concrete routes —
 * a primary plus at most three fallbacks — each carrying a provider, model,
 * optional per-route limits, and the semantic model-role name when the route
 * came from a role reference (legacy inline routes carry `role: null`; a role
 * name is never fabricated). Lookup prefers an explicit purpose entry, then
 * the workflow mode, and only then the legacy first-configured route for
 * non-mode purposes with no explicit entry; a missing workflow-mode route is a
 * defensive error, never a silent substitution. The router normalizes the
 * effective configuration once at construction and is deterministic: no IO,
 * no randomness, same inputs give the same chains.
 */

import type { WorkflowMode } from '../core/modes.js';
import { normalizeRouting } from '../config/load.js';
import type { RelayConfig } from '../config/schema.js';
import type { NormalizedChain, NormalizedRoute, NormalizedRouting } from '../config/normalized.js';
import type { ModelProvider, ProviderName } from '../providers/provider.js';

export type ResolvedRoute = {
  readonly provider: ModelProvider;
  readonly providerName: ProviderName;
  readonly model: string;
  /** Semantic model-role name; null for legacy inline routes. */
  readonly role: string | null;
  readonly limits?: {
    readonly maxOutputTokens?: number;
    readonly temperature?: number;
  };
};

export class RouteUnavailableError extends Error {
  constructor(detail: string) {
    super(`No usable model route: ${detail}`);
    this.name = 'RouteUnavailableError';
  }
}

export type RouteOptions = {
  readonly mode?: WorkflowMode | null;
  readonly purpose: string;
};

function describeKey(options: RouteOptions): string {
  return options.mode !== undefined && options.mode !== null
    ? `mode "${options.mode}" (purpose "${options.purpose}")`
    : `purpose "${options.purpose}"`;
}

export class ModelRouter {
  private readonly providers: ReadonlyMap<ProviderName, ModelProvider>;
  private readonly routing: NormalizedRouting;

  constructor(
    providers: ReadonlyMap<ProviderName, ModelProvider>,
    config: RelayConfig,
  ) {
    this.providers = providers;
    this.routing = normalizeRouting(config);
  }

  /**
   * Resolve the primary route. Lookup order: an explicit purpose entry (today
   * only `intent`), then the workflow mode, then — for non-mode purposes with
   * no explicit entry only — the legacy first-configured route.
   */
  route(options: RouteOptions): ResolvedRoute {
    const chain = this.resolveChain(options);
    return this.primaryOf(this.attachProviders(chain, describeKey(options)), options);
  }

  /** Primary route plus fallbacks, filtered to registered providers. */
  routeChain(options: RouteOptions): readonly ResolvedRoute[] {
    const chain = this.resolveChain(options);
    const resolved = this.attachProviders(chain, describeKey(options));
    this.primaryOf(resolved, options); // fail fast with the same error as route()
    return resolved;
  }

  /**
   * The expanded fallback chain for a named model role (`--role` override).
   * Unknown roles fail with an error listing the available roles.
   */
  chainForRole(role: string): readonly ResolvedRoute[] {
    const chain = this.routing.roleChains.get(role);
    if (chain === undefined) {
      const available = [...this.routing.roleChains.keys()];
      throw new RouteUnavailableError(
        `unknown model role "${role}"; available roles: ` +
          (available.length > 0
            ? available.join(', ')
            : '(no roles are defined in the models catalog)') +
          `. Define a models.${role} entry in relay configuration.`,
      );
    }
    const resolved = this.attachProviders(chain, `role "${role}"`);
    if (resolved.length === 0) {
      throw new RouteUnavailableError(`the chain for role "${role}" is empty`);
    }
    return resolved;
  }

  /**
   * Pick the chain for a call: exact purpose entry first, then the workflow
   * mode, then the legacy first-configured route — but only for non-mode
   * purposes. A mode without a route is a defensive error (effective
   * configuration validation makes it impossible): never borrow another mode.
   */
  private resolveChain(options: RouteOptions): NormalizedChain {
    const purposeChain = this.routing.purposeChains.get(options.purpose);
    if (purposeChain !== undefined) return purposeChain;

    const { mode } = options;
    if (mode !== undefined && mode !== null) {
      const modeChain = this.routing.purposeChains.get(mode);
      if (modeChain !== undefined) return modeChain;
      throw new RouteUnavailableError(
        `no route is configured for mode "${mode}" (purpose "${options.purpose}"). ` +
          'Effective configuration guarantees every workflow mode a route; ' +
          'add a routing entry for the mode in relay configuration.',
      );
    }

    const legacy = this.routing.legacyFallback;
    if (legacy !== null) return legacy;
    throw new RouteUnavailableError(
      `no route is configured for purpose "${options.purpose}" and no fallback route exists. ` +
        'Add a routing entry for the purpose (for example "intent") in relay configuration.',
    );
  }

  /**
   * Attach providers to a normalized chain: the primary provider must be
   * registered (an actionable error otherwise); fallback entries whose
   * provider is unregistered are filtered out, preserving order.
   */
  private attachProviders(chain: NormalizedChain, forWhat: string): readonly ResolvedRoute[] {
    const primary = chain[0];
    if (primary === undefined) {
      throw new RouteUnavailableError(`the configured route for ${forWhat} is empty`);
    }
    const primaryProvider = this.providers.get(primary.provider);
    if (primaryProvider === undefined) {
      const roleSuffix = primary.role !== null ? `, role "${primary.role}"` : '';
      throw new RouteUnavailableError(
        `provider "${primary.provider}" required by the route for ${forWhat}${roleSuffix} is not registered. ` +
          'Register the provider or point the route at an installed provider in relay configuration.',
      );
    }
    const resolved: ResolvedRoute[] = [this.toResolved(primaryProvider, primary)];
    for (const fallback of chain.slice(1)) {
      const provider = this.providers.get(fallback.provider);
      if (provider === undefined) continue;
      resolved.push(this.toResolved(provider, fallback));
    }
    return resolved;
  }

  private primaryOf(chain: readonly ResolvedRoute[], options: RouteOptions): ResolvedRoute {
    const first = chain[0];
    if (first === undefined) {
      throw new RouteUnavailableError(
        `no usable provider for the primary route for ${describeKey(options)}`,
      );
    }
    return first;
  }

  private toResolved(provider: ModelProvider, route: NormalizedRoute): ResolvedRoute {
    const limits =
      route.limits !== undefined
        ? {
            ...(route.limits.maxOutputTokens !== undefined
              ? { maxOutputTokens: route.limits.maxOutputTokens }
              : {}),
            ...(route.limits.temperature !== undefined
              ? { temperature: route.limits.temperature }
              : {}),
          }
        : undefined;
    return {
      provider,
      providerName: route.provider,
      model: route.model,
      role: route.role,
      ...(limits !== undefined && Object.keys(limits).length > 0 ? { limits } : {}),
    };
  }
}
