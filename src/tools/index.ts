/**
 * Default tool registry assembly.
 *
 * Registers the full built-in tool set — filesystem, search, patch, process,
 * and read-only git tools — against one registry wired to the configured
 * tool settings.
 */

import { ToolRegistry } from './registry.js';
import { registerFilesystemTools } from './filesystem.js';
import { registerSearchTools } from './search.js';
import { registerPatchTools } from './patch.js';
import { registerProcessTools } from './process.js';
import { registerGitTools } from './git.js';
import type { ToolSettings } from '../config/schema.js';

export { registerFilesystemTools } from './filesystem.js';
export { registerSearchTools } from './search.js';
export { registerPatchTools } from './patch.js';
export { registerProcessTools } from './process.js';
export { registerGitTools } from './git.js';
export { runGit } from './git.js';
export {
  PathEscapeError,
  isWithinWorkspace,
  normalizeWorkspacePath,
  resolveWorkspacePath,
} from './path-guard.js';
export { boundOutput, envNameLooksSecret, redactSecrets, sanitizeEnvForRecord, SECRET_ENV_PATTERNS } from './redact.js';
export { relativePathArg, boundedString, commonArgs } from './schemas.js';

/** Build a registry containing every built-in tool, configured by `settings`. */
export function createDefaultToolRegistry(settings: ToolSettings): ToolRegistry {
  const registry = new ToolRegistry();
  registerFilesystemTools(registry, settings);
  registerSearchTools(registry, settings);
  registerPatchTools(registry);
  registerProcessTools(registry, settings);
  registerGitTools(registry);
  return registry;
}
