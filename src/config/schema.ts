/**
 * Configuration schema.
 *
 * Relay configuration is YAML, validated strictly: unknown providers, models,
 * modes, or fields are rejected with actionable messages. Configuration never
 * contains credentials — only the names of environment variables that hold
 * them.
 *
 * Routing entries may reference a named model role from the `models` catalog
 * (a role-name string such as `reasoning`) or use the legacy inline form (a
 * provider/model object); both are valid in the same file. The raw
 * string/object union is a configuration-only shape: it is normalized before
 * any runtime use (see `normalizeRouting` in ./load.js and the contract types
 * in ./normalized.js) and must not leak past the configuration boundary.
 */

import { z } from 'zod';
import { WORKFLOW_MODES, type WorkflowMode } from '../core/modes.js';
import { INTENT_PURPOSE } from './normalized.js';

export const PROVIDER_NAMES = ['openai', 'anthropic', 'fake'] as const;
export type ProviderConfigName = (typeof PROVIDER_NAMES)[number];

export const CONFIG_SCHEMA_VERSION = 1;

/** The one canonical role-name rule: lowercase kebab-case, 1-64 characters. */
export const ROLE_NAME_PATTERN = /^[a-z][a-z0-9-]{0,63}$/;

const ROLE_NAME_MESSAGE =
  'role names are lowercase kebab-case, 1-64 characters, e.g. reasoning, ux-writing';

/** A model-role name such as `reasoning` or `security-review`. Not a fixed enum. */
export const roleNameSchema = z.string().refine((value) => ROLE_NAME_PATTERN.test(value), {
  message: ROLE_NAME_MESSAGE,
});

const routeLimitsSchema = z.object({
  maxOutputTokens: z.number().int().min(256).max(1_000_000).optional(),
  temperature: z.number().min(0).max(2).optional(),
});

export const routeSchema = z.object({
  provider: z.enum(PROVIDER_NAMES),
  model: z.string().min(1),
  fallbacks: z
    .array(
      z.object({
        provider: z.enum(PROVIDER_NAMES),
        model: z.string().min(1),
      }),
    )
    .max(3)
    .optional(),
  limits: routeLimitsSchema.optional(),
});

/** A named model-role definition in the `models` catalog. */
export const modelRoleSchema = z.object({
  provider: z.enum(PROVIDER_NAMES),
  model: z.string().min(1),
  /** Role-name references, resolved against the catalog after layers merge. */
  fallbacks: z.array(roleNameSchema).max(3).optional(),
  limits: routeLimitsSchema.optional(),
});

/**
 * The `models` catalog. Keys are user-defined role names; the key check runs
 * as a refinement (rather than a record-key schema) so rejections carry the
 * actionable role-name message instead of zod's generic key diagnostic.
 */
const modelCatalogSchema = z
  .record(z.string(), modelRoleSchema)
  .superRefine((catalog, ctx) => {
    for (const role of Object.keys(catalog)) {
      if (!ROLE_NAME_PATTERN.test(role)) {
        ctx.addIssue({ code: 'custom', path: [role], message: ROLE_NAME_MESSAGE });
      }
    }
  })
  .optional();

/** A routing value: a role-name reference or a legacy inline route object. */
export const routingValueSchema = z.union([roleNameSchema, routeSchema], {
  error: () =>
    'routing values must be a model-role name (a lowercase kebab-case string referencing a models entry) '
    + 'or an inline route object with provider and model',
});

export const budgetLimitsSchema = z.object({
  maxTurns: z.number().int().min(1).max(500).default(24),
  maxModelCalls: z.number().int().min(1).max(2000).default(48),
  maxToolCalls: z.number().int().min(1).max(2000).default(96),
  maxOutputTokens: z.number().int().min(1024).max(10_000_000).default(400_000),
  wallClockMs: z.number().int().min(10_000).max(86_400_000).default(1_800_000),
  maxRepeatedActions: z.number().int().min(2).max(20).default(3),
});

export const providerSettingsSchema = z.object({
  /** Environment variable that holds the API key. The key itself never lives here. */
  apiKeyEnv: z.string().min(1).default('OPENAI_API_KEY'),
  baseUrl: z.string().url().optional(),
});

export const autonomySettingSchema = z.string().refine(
  (value) =>
    value === 'off' || value === 'next-checkpoint' || value === 'workflow'
    || (value.startsWith('until:') && WORKFLOW_MODES.includes(value.slice('until:'.length) as never)),
  { message: 'autonomy must be off, next-checkpoint, workflow, or until:<mode>' },
);

/** Routing keys allowed in a layer: the seven workflow modes plus `intent`. */
const ALLOWED_ROUTING_KEYS: readonly string[] = [...WORKFLOW_MODES, INTENT_PURPOSE];

export const relayConfigSchema = z.object({
  version: z.literal(CONFIG_SCHEMA_VERSION).default(CONFIG_SCHEMA_VERSION),
  providers: z
    .object({
      openai: providerSettingsSchema.default({ apiKeyEnv: 'OPENAI_API_KEY' }),
      anthropic: providerSettingsSchema.default({ apiKeyEnv: 'ANTHROPIC_API_KEY' }),
    })
    .default({ openai: { apiKeyEnv: 'OPENAI_API_KEY' }, anthropic: { apiKeyEnv: 'ANTHROPIC_API_KEY' } }),
  routing: z.record(z.string(), routingValueSchema),
  models: modelCatalogSchema,
  budgets: budgetLimitsSchema,
  autonomy: autonomySettingSchema.default('next-checkpoint'),
  tools: z
    .object({
      commandTimeoutMs: z.number().int().min(1_000).max(600_000).default(120_000),
      maxSearchResults: z.number().int().min(1).max(10_000).default(200),
      maxFileBytes: z.number().int().min(1024).max(20_000_000).default(2_000_000),
    })
    .prefault({}),
});

export type RouteConfig = z.infer<typeof routeSchema>;
export type ModelRoleConfig = z.infer<typeof modelRoleSchema>;
export type BudgetLimits = z.infer<typeof budgetLimitsSchema>;
export type ProviderSettings = z.infer<typeof providerSettingsSchema>;
export type ToolSettings = z.infer<typeof relayConfigSchema>['tools'];

/**
 * A role-name reference inside a routing table: a bare string such as
 * `reasoning`. The never-typed optional route fields exist only so legacy
 * typed consumers of parsed routing (which read route fields off union
 * values) keep compiling; they are never present at runtime on a reference.
 */
export type RoleReference = string & {
  readonly provider?: never;
  readonly model?: never;
  readonly fallbacks?: never;
  readonly limits?: never;
};

/** Raw parsed routing value: a role reference or a legacy inline route. */
export type RoutingValue = RoleReference | RouteConfig;

/** Raw parsed routing table. Configuration-only; normalize before runtime use. */
export type RoutingTable = Partial<Record<WorkflowMode | typeof INTENT_PURPOSE, RoutingValue>> &
  Record<string, RoutingValue>;

export type RelayConfig = {
  version: typeof CONFIG_SCHEMA_VERSION;
  providers: { openai: ProviderSettings; anthropic: ProviderSettings };
  routing: RoutingTable;
  /** Named model-role catalog; absent in legacy inline-only configurations. */
  models?: Record<string, ModelRoleConfig>;
  budgets: BudgetLimits;
  autonomy: string;
  tools: ToolSettings;
};

export class ConfigValidationError extends Error {
  constructor(
    readonly source: string,
    readonly issues: readonly string[],
  ) {
    super(
      `Invalid Relay configuration in ${source}:\n${issues.map((issue) => `  - ${issue}`).join('\n')}`,
    );
    this.name = 'ConfigValidationError';
  }
}

export function formatConfigIssues(error: z.ZodError): string[] {
  return error.issues.map((issue) => `${issue.path.join('.') || '(root)'}: ${issue.message}`);
}

export function parseRelayConfig(source: string, text: unknown): RelayConfig {
  const parsed = relayConfigSchema.safeParse(text);
  if (!parsed.success) {
    throw new ConfigValidationError(source, formatConfigIssues(parsed.error));
  }
  const config = parsed.data;
  const unknownKeys = Object.keys(config.routing).filter(
    (key) => !ALLOWED_ROUTING_KEYS.includes(key),
  );
  if (unknownKeys.length > 0) {
    throw new ConfigValidationError(
      source,
      [
        `routing contains unknown modes: ${unknownKeys.join(', ')} `
          + `(allowed keys: ${WORKFLOW_MODES.join(', ')}, and ${INTENT_PURPOSE})`,
      ],
    );
  }
  const missingModes = WORKFLOW_MODES.filter((mode) => !config.routing[mode]);
  if (missingModes.length > 0) {
    throw new ConfigValidationError(
      source,
      [`routing is missing modes: ${missingModes.join(', ')}`],
    );
  }
  return config as RelayConfig;
}
