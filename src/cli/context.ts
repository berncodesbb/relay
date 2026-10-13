/**
 * CLI composition root.
 *
 * Assembles the configuration, storage, artifact, tool, provider, and workflow
 * pieces every stateful command needs. Databases are opened (and `.relay/`
 * created) only here — commands that never need state (help, version,
 * `config show|path|get`) load configuration directly and create nothing on
 * disk.
 *
 * The workflow controller module is imported lazily inside
 * {@link createController} so commands that do not touch the workflow still
 * run while the runtime surface is being integrated.
 */

import { existsSync } from 'node:fs';
import type { RelayConfig } from '../config/schema.js';
import { loadConfig, type LoadedConfig } from '../config/load.js';
import { replayProject } from '../core/reducer.js';
import { ArtifactMarkdownStore } from '../artifacts/markdown-store.js';
import { AnthropicProvider } from '../providers/anthropic.js';
import { FakeProvider } from '../providers/fake.js';
import { OpenAIProvider } from '../providers/openai.js';
import type { ModelProvider, ProviderName } from '../providers/provider.js';
import { ModelRouter } from '../runtime/model-router.js';
import type { WorkflowController } from '../runtime/controller.js';
import { openDatabase, type RelayDatabase } from '../storage/database.js';
import { SqliteEventStore } from '../storage/event-store.js';
import { runMigrations } from '../storage/migrations.js';
import { relayPaths } from '../storage/paths.js';
import { ProjectRepository } from '../storage/project-repository.js';
import { createDefaultToolRegistry } from '../tools/index.js';
import type { ToolRegistry } from '../tools/registry.js';

export type CliContext = {
  readonly workspaceRoot: string;
  readonly config: LoadedConfig;
  readonly db: RelayDatabase;
  readonly eventStore: SqliteEventStore;
  readonly repository: ProjectRepository;
  readonly artifacts: ArtifactMarkdownStore;
  readonly tools: ToolRegistry;
  close(): void;
};

/** `--provider` / `--model` / `--role` values carried from the parsed command line. */
type InvocationOverrides = { provider?: ProviderName; model?: string; role?: string };

const controllerOverrides = new WeakMap<CliContext, InvocationOverrides>();

/** Provider names the CLI accepts for `--provider`. */
export const CLI_PROVIDER_OVERRIDES: readonly ProviderName[] = ['openai', 'anthropic', 'fake'];

/**
 * Raised when a `--role` value is absent from the effective `models` catalog.
 * `relay run` validates the role before creating project state, so this is a
 * defensive guard at controller-construction time, not the user-facing path.
 */
export class UnknownModelRoleError extends Error {
  constructor(
    readonly role: string,
    readonly available: readonly string[],
  ) {
    super(
      `unknown model role "${role}"; available roles: `
        + (available.length > 0 ? available.join(', ') : '(none defined)'),
    );
    this.name = 'UnknownModelRoleError';
  }
}

/**
 * Build the full stateful context: layered configuration, an open (and
 * migrated) `.relay/state.db`, the event store and project repository, the
 * artifact store, and the default tool registry. Creating the context creates
 * the `.relay/` directory when absent.
 */
export function createCliContext(options: {
  workspaceRoot: string;
  providerOverride?: 'openai' | 'anthropic' | 'fake';
  modelOverride?: string;
  roleOverride?: string;
}): CliContext {
  const forceProvider = options.providerOverride ?? (process.env.RELAY_FAKE === '1' ? 'fake' : undefined);
  const loaded = loadConfig({
    workspaceRoot: options.workspaceRoot,
    overrides: forceProvider === undefined ? undefined : { forceProvider },
  });

  const paths = relayPaths(options.workspaceRoot);
  const db = openDatabase(paths.database);
  runMigrations(db);
  const eventStore = new SqliteEventStore(db);
  const repository = new ProjectRepository(db, eventStore, replayProject);
  const artifacts = new ArtifactMarkdownStore(paths.artifactsDir);
  const tools = createDefaultToolRegistry(loaded.config.tools);

  const context: CliContext = {
    workspaceRoot: options.workspaceRoot,
    config: loaded,
    db,
    eventStore,
    repository,
    artifacts,
    tools,
    close(): void {
      db.close();
    },
  };

  const overrides: InvocationOverrides = { ...(controllerOverrides.get(context) ?? {}) };
  if (options.providerOverride !== undefined) overrides.provider = options.providerOverride;
  if (options.modelOverride !== undefined) overrides.model = options.modelOverride;
  if (options.roleOverride !== undefined) overrides.role = options.roleOverride;
  controllerOverrides.set(context, overrides);
  return context;
}

/**
 * Build a workflow controller over an existing context. The controller module
 * is loaded dynamically so lighter commands keep working independently of it.
 */
export async function createController(
  context: CliContext,
  options?: { signal?: AbortSignal; sessionId?: string },
): Promise<WorkflowController> {
  const { WorkflowController } = await import('../runtime/controller.js');
  const config = context.config.config;
  const fakeForced =
    controllerOverrides.get(context)?.provider === 'fake' || process.env.RELAY_FAKE === '1';
  const providers = await buildProviders(config, fakeForced);
  const router = new ModelRouter(providers, config);

  const role = controllerOverrides.get(context)?.role;
  if (role !== undefined && (config.models ?? {})[role] === undefined) {
    // Defensive: `relay run` validates --role against the same effective
    // configuration before creating the context, so this should be
    // unreachable — surface an actionable error rather than routing on.
    const available = Object.keys(config.models ?? {});
    throw new UnknownModelRoleError(role, available);
  }

  const constructorOptions: {
    sessionId?: string;
    providerOverrides?: { provider?: ProviderName; model?: string };
    roleOverride?: string;
    signal?: AbortSignal;
  } = {};
  if (options?.sessionId !== undefined) constructorOptions.sessionId = options.sessionId;
  if (options?.signal !== undefined) constructorOptions.signal = options.signal;
  if (role !== undefined) constructorOptions.roleOverride = role;
  const overrides = controllerOverrides.get(context);
  if (overrides !== undefined && (overrides.provider !== undefined || overrides.model !== undefined)) {
    const providerOverrides: { provider?: ProviderName; model?: string } = {};
    if (overrides.provider !== undefined) providerOverrides.provider = overrides.provider;
    if (overrides.model !== undefined) providerOverrides.model = overrides.model;
    constructorOptions.providerOverrides = providerOverrides;
  }

  const controller = new WorkflowController(
    {
      workspaceRoot: context.workspaceRoot,
      repository: context.repository,
      eventStore: context.eventStore,
      artifacts: context.artifacts,
      router,
      tools: context.tools,
      budgets: config.budgets,
    },
    constructorOptions,
  );
  return controller;
}

/** Whether this workspace already has a `.relay/state.db` (i.e. was initialized). */
export function hasRelayState(workspaceRoot: string): boolean {
  return existsSync(relayPaths(workspaceRoot).database);
}

/**
 * Register the providers available to the router: always `fake`; `openai` and
 * `anthropic` only when their configured credential environment variable is
 * non-empty, so routing never fails late inside an SDK call.
 */
async function buildProviders(
  config: RelayConfig,
  preferDemoFake: boolean,
): Promise<ReadonlyMap<ProviderName, ModelProvider>> {
  const providers = new Map<ProviderName, ModelProvider>();
  providers.set('fake', preferDemoFake ? await createDemoFakeProvider() : new FakeProvider());

  const openaiKey = (process.env[config.providers.openai.apiKeyEnv] ?? '').trim();
  if (openaiKey.length > 0) {
    providers.set('openai', new OpenAIProvider(providerOptions(openaiKey, config.providers.openai.baseUrl)));
  }
  const anthropicKey = (process.env[config.providers.anthropic.apiKeyEnv] ?? '').trim();
  if (anthropicKey.length > 0) {
    providers.set(
      'anthropic',
      new AnthropicProvider(providerOptions(anthropicKey, config.providers.anthropic.baseUrl)),
    );
  }
  return providers;
}

function providerOptions(apiKey: string, baseUrl: string | undefined): { apiKey: string; baseUrl?: string } {
  return baseUrl === undefined ? { apiKey } : { apiKey, baseUrl };
}

/**
 * Prefer the runtime's scripted demo provider when it exists (used for
 * offline demos and tests); fall back to the plain echo fake provider.
 */
async function createDemoFakeProvider(): Promise<ModelProvider> {
  try {
    const mod = (await import('../runtime/demo-provider.js')) as unknown as Record<string, unknown>;
    for (const name of ['createDemoProvider', 'buildDemoProvider', 'createDemoFakeProvider', 'demoProvider']) {
      const factory = mod[name];
      if (typeof factory !== 'function') continue;
      try {
        const built = (factory as () => unknown)();
        if (isModelProvider(built)) return built;
      } catch {
        // Try the next candidate export shape.
      }
    }
  } catch {
    // Demo provider not present (yet); the echo fake provider is fine.
  }
  return new FakeProvider();
}

function isModelProvider(value: unknown): value is ModelProvider {
  return (
    typeof value === 'object' &&
    value !== null &&
    typeof (value as { name?: unknown }).name === 'string' &&
    typeof (value as { stream?: unknown }).stream === 'function'
  );
}
