/**
 * `relay config [show|path|get <key>]` — inspect effective configuration.
 *
 * Read-only: this command never creates `.relay/` or any other file. The
 * configuration itself holds no secrets — only the names of the environment
 * variables that hold credentials — so nothing needs redacting.
 */

import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { stringify as stringifyYaml } from 'yaml';
import {
  PROJECT_CONFIG_FILENAME,
  USER_CONFIG_PATH,
  loadConfig,
} from '../../config/load.js';
import type { OutputWriter } from '../output.js';

export async function runConfig(options: {
  workspaceRoot: string;
  action: 'show' | 'path' | 'get';
  key: string | undefined;
  output: OutputWriter;
}): Promise<number> {
  const { workspaceRoot, action, key, output } = options;
  const loaded = loadConfig({ workspaceRoot });

  if (action === 'show') {
    const layers = loaded.layers
      .map((layer) => (layer.path !== null ? `${layer.source} (${layer.path})` : layer.source))
      .join(' < ');
    output.text('# effective config (defaults < user < project)');
    output.text(`# layers: ${layers}`);
    output.text(stringifyYaml(loaded.config).trimEnd());
    return 0;
  }

  if (action === 'path') {
    const projectPath = join(workspaceRoot, PROJECT_CONFIG_FILENAME);
    output.text(`user:    ${USER_CONFIG_PATH} ${existsSync(USER_CONFIG_PATH) ? '(exists)' : '(missing)'}`);
    output.text(`project: ${projectPath} ${existsSync(projectPath) ? '(exists)' : '(missing)'}`);
    return 0;
  }

  // action === 'get' (argument parsing guarantees a key)
  const lookup = lookupDotted(loaded.config, key ?? '');
  if (!lookup.found) {
    output.error(`unknown key: ${key}`);
    return 1;
  }
  const value = lookup.value;
  if (typeof value === 'string') output.text(value);
  else if (typeof value === 'number' || typeof value === 'boolean') output.text(String(value));
  else output.text(stringifyYaml(value).trimEnd());
  return 0;
}

function lookupDotted(source: unknown, key: string): { found: boolean; value: unknown } {
  let current: unknown = source;
  for (const part of key.split('.').filter((piece) => piece.length > 0)) {
    if (typeof current !== 'object' || current === null) return { found: false, value: undefined };
    const record = current as Record<string, unknown>;
    if (!(part in record)) return { found: false, value: undefined };
    current = record[part];
  }
  return { found: true, value: current };
}
