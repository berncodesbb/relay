/**
 * Normalized routing contract.
 *
 * Shared seam between configuration and runtime: configuration may describe
 * routes as role references or legacy inline objects, but by the time the
 * model router runs, everything is normalized into the concrete shapes here.
 * The raw string/object union never leaks past this boundary.
 */

import type { ProviderConfigName, RouteConfig } from './schema.js';

/** One concrete executable route. `role` is null for legacy inline routes. */
export type NormalizedRoute = {
  readonly role: string | null;
  readonly provider: ProviderConfigName;
  readonly model: string;
  readonly limits: RouteConfig['limits'];
};

/**
 * Ordered execution chain: primary first, fallbacks after, deduplicated
 * without reordering, at most one primary plus three fallbacks.
 */
export type NormalizedChain = readonly NormalizedRoute[];

/**
 * The runtime-facing view of an effective configuration's routing.
 *
 * - `roleChains`: every defined model role (user-defined names included),
 *   expanded to its full fallback chain.
 * - `purposeChains`: routing keys (the seven workflow modes plus explicit
 *   non-mode purposes such as `intent`), each resolved to a chain. Legacy
 *   inline entries appear here with `role: null`.
 * - `legacyFallback`: the first configured route in configuration order,
 *   used ONLY for non-mode purposes that have no explicit routing entry —
 *   preserving the historical behavior.
 */
export type NormalizedRouting = {
  readonly roleChains: ReadonlyMap<string, NormalizedChain>;
  readonly purposeChains: ReadonlyMap<string, NormalizedChain>;
  readonly legacyFallback: NormalizedChain | null;
};

/** Maximum fallbacks after the primary route (existing behavior). */
export const MAX_FALLBACKS = 3;

/** The only non-mode routing purpose with a dedicated configuration key. */
export const INTENT_PURPOSE = 'intent';
