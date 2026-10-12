/**
 * Configuration schema.
 *
 * Relay configuration is YAML, validated strictly: unknown providers, models,
 * modes, or fields are rejected with actionable messages. Configuration never
 * contains credentials — only the names of environment variables that hold
 * them.
 */

import { z } from 'zod';
import { WORKFLOW_MODES } from '../core/modes.js';

export const PROVIDER_NAMES = ['openai', 'anthropic', 'fake'] as const;
export type ProviderConfigName = (typeof PROVIDER_NAMES)[number];

export const CONFIG_SCHEMA_VERSION = 1;

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
  limits: z
    .object({
      maxOutputTokens: z.number().int().min(256).max(1_000_000).optional(),
      temperature: z.number().min(0).max(2).optional(),
    })
    .optional(),
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

export const relayConfigSchema = z.object({
  version: z.literal(CONFIG_SCHEMA_VERSION).default(CONFIG_SCHEMA_VERSION),
  providers: z
    .object({
      openai: providerSettingsSchema.default({ apiKeyEnv: 'OPENAI_API_KEY' }),
      anthropic: providerSettingsSchema.default({ apiKeyEnv: 'ANTHROPIC_API_KEY' }),
    })
    .default({ openai: { apiKeyEnv: 'OPENAI_API_KEY' }, anthropic: { apiKeyEnv: 'ANTHROPIC_API_KEY' } }),
  routing: z.record(z.string(), routeSchema),
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
export type BudgetLimits = z.infer<typeof budgetLimitsSchema>;
export type ProviderSettings = z.infer<typeof providerSettingsSchema>;
export type ToolSettings = z.infer<typeof relayConfigSchema>['tools'];

export type RelayConfig = {
  version: typeof CONFIG_SCHEMA_VERSION;
  providers: { openai: ProviderSettings; anthropic: ProviderSettings };
  routing: Partial<Record<(typeof WORKFLOW_MODES)[number], RouteConfig>> & Record<string, RouteConfig>;
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
  const unknownModes = Object.keys(config.routing).filter(
    (mode) => !(WORKFLOW_MODES as readonly string[]).includes(mode),
  );
  if (unknownModes.length > 0) {
    throw new ConfigValidationError(
      source,
      [`routing contains unknown modes: ${unknownModes.join(', ')}`],
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
