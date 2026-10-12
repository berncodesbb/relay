/**
 * Configuration loading and precedence.
 *
 * defaults < user config (`~/.config/relay/config.yaml`) < project config
 * (`relay.config.yaml` in the workspace root) < programmatic overrides.
 * Later layers replace routing tables wholesale; merging happens at the
 * top-level key granularity.
 */

import { existsSync, readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { isAbsolute, join } from 'node:path';
import { parse as parseYaml } from 'yaml';
import { DEFAULT_CONFIG } from './defaults.js';
import { parseRelayConfig, type RelayConfig } from './schema.js';

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

function readYamlLayer(path: string, layer: ConfigLayer['source'], merged: RelayConfig): RelayConfig {
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
  // Top-level merge: later layers replace whole sections.
  return {
    ...merged,
    ...layerConfig,
    version: merged.version,
    providers: { ...merged.providers, ...layerConfig.providers },
    routing: { ...merged.routing, ...layerConfig.routing },
    budgets: { ...merged.budgets, ...layerConfig.budgets },
    tools: { ...merged.tools, ...layerConfig.tools },
  };
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
  let config: RelayConfig = DEFAULT_CONFIG;

  if (existsSync(userPath)) {
    config = readYamlLayer(userPath, 'user', config);
    layers.push({ source: 'user', path: userPath });
  }
  if (projectPath !== userPath && existsSync(projectPath)) {
    config = readYamlLayer(projectPath, 'project', config);
    layers.push({ source: 'project', path: projectPath });
  }

  if (overrides?.autonomy !== undefined) {
    config = { ...config, autonomy: overrides.autonomy };
    layers.push({ source: 'overrides', path: null });
  }
  if (overrides?.forceProvider) {
    const provider = overrides.forceProvider;
    const routing: RelayConfig['routing'] = {};
    for (const [mode, route] of Object.entries(config.routing)) {
      routing[mode] = { ...route!, provider, model: route!.model };
    }
    config = { ...config, routing };
    layers.push({ source: 'overrides', path: null });
  }

  return { config, layers };
}
