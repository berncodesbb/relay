/**
 * Configuration loading and precedence.
 *
 * defaults < user config (`~/.config/relay/config.yaml`) < project config
 * (`relay.config.yaml` in the workspace root) < programmatic overrides.
 * Routing entries merge by key; the `models` role catalog merges by role name
 * so a later layer replaces one complete role definition while unrelated
 * roles from lower layers survive. After merging, the effective configuration
 * is cross-referenced (every role reference and fallback role must resolve,
 * no self-references or cycles, expanded chains within one primary plus
 * MAX_FALLBACKS) before it is returned, and can be normalized into the
 * runtime-facing routing contract with `normalizeRouting`.
 */

import { existsSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { isAbsolute, join } from 'node:path';
import { parse as parseYaml } from 'yaml';
import {
  MAX_FALLBACKS,
  type NormalizedChain,
  type NormalizedRoute,
  type NormalizedRouting,
} from './normalized.js';
import { DEFAULT_CONFIG } from './defaults.js';
import {
  ConfigValidationError,
  parseRelayConfig,
  type ModelRoleConfig,
  type RelayConfig,
  type RouteConfig,
  type RoutingTable,
} from './schema.js';

export type ConfigLayer = {
  readonly source: 'defaults' | 'user' | 'project' | 'overrides';
  readonly path: string | null;
};

export type LoadedConfig = {
  readonly config: RelayConfig;
  /** Layers in precedence order, lowest first; null path for defaults. */
  readonly layers: readonly ConfigLayer[];
};

export const USER_CONFIG_PATH = join(homedir(), '.config', 'relay', 'config.yaml');
export const PROJECT_CONFIG_FILENAME = 'relay.config.yaml';

export type ConfigOverrides = Partial<Pick<RelayConfig, 'autonomy'>> & {
  /** Force one provider everywhere, used by `--provider` and tests. */
  readonly forceProvider?: 'openai' | 'anthropic' | 'fake' | undefined;
};

/** Fallback error-source label when no layer file is known for a piece. */
export const EFFECTIVE_CONFIG_SOURCE = 'the effective configuration';

/**
 * Provenance for the effective configuration: which layer label (usually a
 * file path) last defined each routing entry and each model role. Used to
 * attribute cross-reference errors to the layer that introduced the offending
 * piece instead of blaming the whole effective configuration.
 */
export type EffectiveConfigOrigins = {
  readonly routing: ReadonlyMap<string, string>;
  readonly models: ReadonlyMap<string, string>;
};

type MutableOrigins = { routing: Map<string, string>; models: Map<string, string> };

function readYamlLayer(path: string, merged: RelayConfig, origins: MutableOrigins): RelayConfig {
  if (!existsSync(path)) return merged;
  let text: string;
  try {
    text = readFileSync(path, 'utf8');
  } catch (error) {
    throw new Error(`Cannot read Relay configuration at ${path}: ${(error as Error).message}`);
  }
  let parsed: unknown;
  try {
    parsed = text.trim() === '' ? {} : parseYaml(text);
  } catch (error) {
    throw new Error(`Cannot parse YAML in ${path}: ${(error as Error).message}`);
  }
  const layerConfig = parseRelayConfig(path, parsed);
  // Top-level merge: later layers replace whole sections, except routing
  // (by key) and the models catalog (by role name).
  const next: RelayConfig = {
    ...merged,
    ...layerConfig,
    version: merged.version,
    providers: { ...merged.providers, ...layerConfig.providers },
    routing: { ...merged.routing, ...layerConfig.routing },
    budgets: { ...merged.budgets, ...layerConfig.budgets },
    tools: { ...merged.tools, ...layerConfig.tools },
  };
  for (const key of Object.keys(layerConfig.routing)) {
    origins.routing.set(key, path);
  }
  if (layerConfig.models !== undefined) {
    next.models = { ...(merged.models ?? {}), ...layerConfig.models };
    for (const role of Object.keys(layerConfig.models)) {
      origins.models.set(role, path);
    }
  }
  return next;
}

export function loadConfig(options: {
  workspaceRoot: string;
  userConfigPath?: string;
  overrides?: ConfigOverrides | undefined;
}): LoadedConfig {
  const { workspaceRoot, overrides } = options;
  const userPath = options.userConfigPath ?? USER_CONFIG_PATH;
  const projectPath = isAbsolute(workspaceRoot)
    ? join(workspaceRoot, PROJECT_CONFIG_FILENAME)
    : join(process.cwd(), workspaceRoot, PROJECT_CONFIG_FILENAME);

  const layers: ConfigLayer[] = [{ source: 'defaults', path: null }];
  const origins: MutableOrigins = { routing: new Map(), models: new Map() };
  let config: RelayConfig = DEFAULT_CONFIG;

  if (existsSync(userPath)) {
    config = readYamlLayer(userPath, config, origins);
    layers.push({ source: 'user', path: userPath });
  }
  if (projectPath !== userPath && existsSync(projectPath)) {
    config = readYamlLayer(projectPath, config, origins);
    layers.push({ source: 'project', path: projectPath });
  }

  // The effective configuration always exposes a models catalog, even when
  // every file layer is a legacy inline-only configuration.
  if (config.models === undefined) {
    config = { ...config, models: {} };
  }
  validateEffectiveConfig(config, EFFECTIVE_CONFIG_SOURCE, origins);

  if (overrides?.autonomy !== undefined) {
    config = { ...config, autonomy: overrides.autonomy };
    layers.push({ source: 'overrides', path: null });
  }
  if (overrides?.forceProvider) {
    config = applyForcedProvider(config, overrides.forceProvider);
    layers.push({ source: 'overrides', path: null });
  }

  return { config, layers };
}

/**
 * Validate the effective (merged) configuration's cross-references. Layer
 * parsing already validated each layer's own shape (providers, limits,
 * inline fallback objects); this catches problems that only exist once
 * layers combine: unknown role references, unknown fallback roles,
 * self-references, cycles, and expanded chains longer than one primary plus
 * MAX_FALLBACKS. Throws {@link ConfigValidationError} naming the offending
 * dotted path; when `origins` names the layer file that introduced the piece,
 * that file is used as the error source.
 */
export function validateEffectiveConfig(
  config: RelayConfig,
  source: string,
  origins?: EffectiveConfigOrigins | undefined,
): void {
  const models = config.models ?? {};
  const roleNames = Object.keys(models);
  const availableRoles =
    roleNames.length > 0 ? roleNames.join(', ') : '(no roles are defined in the models catalog)';
  const routingSource = (key: string): string => origins?.routing.get(key) ?? source;
  const roleSource = (role: string): string => origins?.models.get(role) ?? source;

  for (const [key, value] of Object.entries(config.routing)) {
    if (typeof value !== 'string') continue;
    if (models[value] === undefined) {
      throw new ConfigValidationError(routingSource(key), [
        `routing.${key}: references unknown model role "${value}"; available roles: ${availableRoles}. `
          + `Define a models.${value} entry or route "${key}" with an inline provider/model object.`,
      ]);
    }
  }

  for (const [role, definition] of Object.entries(models)) {
    const fallbacks = definition.fallbacks ?? [];
    for (let index = 0; index < fallbacks.length; index += 1) {
      const fallback = fallbacks[index]!;
      if (models[fallback] === undefined) {
        throw new ConfigValidationError(roleSource(role), [
          `models.${role}.fallbacks[${index}]: references unknown model role "${fallback}"; `
            + `available roles: ${availableRoles}`,
        ]);
      }
      if (fallback === role) {
        throw new ConfigValidationError(roleSource(role), [
          `models.${role}.fallbacks[${index}]: role "${role}" cannot fall back to itself`,
        ]);
      }
    }
  }

  detectFallbackCycles(models, roleSource);

  // Excessive chains: every routing-referenced role is checked through its
  // routing key (actionable `routing.<key>` path)...
  const referencedRoles = new Set<string>();
  for (const [key, value] of Object.entries(config.routing)) {
    if (typeof value !== 'string') continue;
    referencedRoles.add(value);
    const chain = expandRoleNames(value, models);
    if (chain.length > 1 + MAX_FALLBACKS) {
      throw new ConfigValidationError(routingSource(key), [
        `routing.${key}: role "${value}" expands to ${chain.length} routes (${chain.join(' -> ')}); `
          + `the maximum is 1 primary plus ${MAX_FALLBACKS} fallbacks`,
      ]);
    }
  }
  // ...and every remaining catalog role too: an unreferenced role is still
  // reachable at runtime through `--role`, so an excessive chain must fail
  // here rather than silently truncate during normalization.
  for (const role of Object.keys(models)) {
    if (referencedRoles.has(role)) continue;
    const chain = expandRoleNames(role, models);
    if (chain.length > 1 + MAX_FALLBACKS) {
      throw new ConfigValidationError(roleSource(role), [
        `models.${role}: role "${role}" expands to ${chain.length} routes (${chain.join(' -> ')}); `
          + `the maximum is 1 primary plus ${MAX_FALLBACKS} fallbacks`,
      ]);
    }
  }
}

/** Depth-first cycle detection over the role fallback graph. */
function detectFallbackCycles(
  models: Record<string, ModelRoleConfig>,
  roleSource: (role: string) => string,
): void {
  const IN_PROGRESS = 1;
  const DONE = 2;
  const state = new Map<string, number>();
  const path: string[] = [];

  const visit = (role: string): void => {
    const current = state.get(role) ?? 0;
    if (current === IN_PROGRESS) {
      const start = Math.max(path.indexOf(role), 0);
      const cycle = [...path.slice(start), role].join(' -> ');
      const origin = path[start] ?? role;
      throw new ConfigValidationError(roleSource(origin), [
        `models.${origin}.fallbacks: role fallback cycle detected (${cycle}); `
          + 'remove the cycle in the models catalog',
      ]);
    }
    if (current === DONE) return;
    state.set(role, IN_PROGRESS);
    path.push(role);
    for (const fallback of models[role]?.fallbacks ?? []) {
      visit(fallback);
    }
    path.pop();
    state.set(role, DONE);
  };

  for (const role of Object.keys(models)) {
    visit(role);
  }
}

/**
 * Expand a primary role through the role fallback graph in breadth-first
 * discovery order, deduplicating while preserving first-seen order. `cap`
 * (when given) hard-stops the chain at that many roles.
 */
function expandRoleNames(
  primary: string,
  models: Record<string, ModelRoleConfig>,
  cap?: number | undefined,
): string[] {
  const order: string[] = [];
  const seen = new Set<string>();
  if (models[primary] === undefined) return order;
  order.push(primary);
  seen.add(primary);
  const queue = [primary];
  while (queue.length > 0) {
    const role = queue.shift()!;
    for (const fallback of models[role]?.fallbacks ?? []) {
      if (seen.has(fallback) || models[fallback] === undefined) continue;
      if (cap !== undefined && order.length >= cap) return order;
      seen.add(fallback);
      order.push(fallback);
      queue.push(fallback);
    }
  }
  return order;
}

/**
 * Normalize the effective configuration into the runtime-facing routing
 * contract: every catalog role expanded to its fallback chain, every routing
 * key resolved to a chain (role references through the role graph, legacy
 * inline objects to themselves plus their inline fallbacks with `role: null`
 * — a role name is never fabricated), and the legacy first-route fallback.
 * Pure and deterministic: no IO, safe to call repeatedly.
 */
export function normalizeRouting(config: RelayConfig): NormalizedRouting {
  const models = config.models ?? {};
  const roleChains = new Map<string, NormalizedChain>();
  for (const role of Object.keys(models)) {
    roleChains.set(role, roleChain(role, models));
  }
  const purposeChains = new Map<string, NormalizedChain>();
  for (const [key, value] of Object.entries(config.routing)) {
    purposeChains.set(
      key,
      typeof value === 'string' ? roleChain(value, models) : inlineChain(value),
    );
  }
  const firstKey = Object.keys(config.routing)[0];
  const legacyFallback = firstKey === undefined ? null : purposeChains.get(firstKey) ?? null;
  return { roleChains, purposeChains, legacyFallback };
}

function roleChain(primary: string, models: Record<string, ModelRoleConfig>): NormalizedChain {
  const chain: NormalizedRoute[] = [];
  for (const name of expandRoleNames(primary, models, 1 + MAX_FALLBACKS)) {
    const definition = models[name];
    if (definition === undefined) continue;
    chain.push({
      role: name,
      provider: definition.provider,
      model: definition.model,
      limits: definition.limits,
    });
  }
  return chain;
}

function inlineChain(route: RouteConfig): NormalizedChain {
  const chain: NormalizedRoute[] = [
    { role: null, provider: route.provider, model: route.model, limits: route.limits },
  ];
  for (const fallback of route.fallbacks ?? []) {
    chain.push({ role: null, provider: fallback.provider, model: fallback.model, limits: undefined });
  }
  return chain;
}

/**
 * Rewrite `--provider`/`RELAY_FAKE` across both routing syntaxes: every models
 * catalog entry and every inline routing object (including inline fallback
 * objects) moves to the forced provider while model ids, limits, and
 * role-name fallback references stay untouched.
 */
function applyForcedProvider(
  config: RelayConfig,
  provider: NonNullable<ConfigOverrides['forceProvider']>,
): RelayConfig {
  const models: Record<string, ModelRoleConfig> = {};
  for (const [role, definition] of Object.entries(config.models ?? {})) {
    models[role] = { ...definition, provider };
  }
  const routing: RoutingTable = {};
  for (const [key, value] of Object.entries(config.routing)) {
    if (typeof value === 'string') {
      routing[key] = value;
      continue;
    }
    routing[key] = {
      ...value,
      provider,
      ...(value.fallbacks !== undefined
        ? { fallbacks: value.fallbacks.map((fallback) => ({ ...fallback, provider })) }
        : {}),
    };
  }
  return { ...config, models, routing };
}
