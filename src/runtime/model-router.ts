/**
 * Fixed mode-based routing with fallback chains.
 *
 * Routing is configuration, not model output: each workflow mode maps to a
 * provider, model, optional fallback order, and per-route limits. The router
 * resolves a primary route plus its fallbacks, keeping only providers that are
 * actually registered. Non-mode purposes (for example "intent" or "status"
 * summaries) intentionally fall back to the first configured route so utility
 * model calls never need their own routing entry.
 */

import type { WorkflowMode } from '../core/modes.js';
import type { RelayConfig, RouteConfig } from '../config/schema.js';
import type { ModelProvider, ProviderName } from '../providers/provider.js';

export type ResolvedRoute = {
  readonly provider: ModelProvider;
  readonly providerName: ProviderName;
  readonly model: string;
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
  private readonly routing: RelayConfig['routing'];

  constructor(
    providers: ReadonlyMap<ProviderName, ModelProvider>,
    routing: RelayConfig['routing'],
  ) {
    this.providers = providers;
    this.routing = routing;
  }

  /**
   * Resolve the primary route. The routing key is `mode` when provided and
   * configured; otherwise (non-mode purposes such as "intent" or "status")
   * the first configured route is used.
   */
  route(options: RouteOptions): ResolvedRoute {
    const config = this.routeConfig(options);
    const provider = this.providers.get(config.provider);
    if (provider === undefined) {
      throw new RouteUnavailableError(
        `provider "${config.provider}" required by the route for ${describeKey(options)} is not registered. ` +
          'Register the provider or point the route at an installed provider in relay configuration.',
      );
    }
    return this.toResolved(provider, config);
  }

  /** Primary route plus fallbacks, filtered to registered providers. */
  routeChain(options: RouteOptions): readonly ResolvedRoute[] {
    const config = this.routeConfig(options);
    const chain: ResolvedRoute[] = [this.route(options)];
    for (const fallback of config.fallbacks ?? []) {
      const provider = this.providers.get(fallback.provider);
      if (provider === undefined) continue;
      chain.push({ provider, providerName: fallback.provider, model: fallback.model });
    }
    return chain;
  }

  private routeConfig(options: RouteOptions): RouteConfig {
    const { mode } = options;
    const key =
      mode !== undefined && mode !== null && this.routing[mode] !== undefined
        ? mode
        : this.firstConfiguredKey();
    const config = key !== null ? this.routing[key] : undefined;
    if (config === undefined) {
      throw new RouteUnavailableError(
        `no route is configured for ${describeKey(options)} and no fallback route exists. ` +
          'Add a routing entry for the mode in relay configuration.',
      );
    }
    return config;
  }

  /** First key in configuration order; used for non-mode purposes. */
  private firstConfiguredKey(): string | null {
    const keys = Object.keys(this.routing);
    return keys.length > 0 ? (keys[0] as string) : null;
  }

  private toResolved(provider: ModelProvider, config: RouteConfig): ResolvedRoute {
    const limits =
      config.limits !== undefined
        ? {
            ...(config.limits.maxOutputTokens !== undefined
              ? { maxOutputTokens: config.limits.maxOutputTokens }
              : {}),
            ...(config.limits.temperature !== undefined
              ? { temperature: config.limits.temperature }
              : {}),
          }
        : undefined;
    return {
      provider,
      providerName: config.provider,
      model: config.model,
      ...(limits !== undefined && Object.keys(limits).length > 0 ? { limits } : {}),
    };
  }
}
