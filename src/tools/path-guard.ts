/**
 * Workspace path boundary enforcement.
 *
 * Every tool resolves user/model-supplied paths through `resolveWorkspacePath`,
 * which rejects traversal, absolute paths, and home shortcuts, then resolves
 * symlinks on the root and on every existing ancestor of the target so a link
 * inside the workspace cannot escape it.
 */

import { realpathSync, statSync } from 'node:fs';
import path from 'node:path';

/** A path argument tried to leave the authorized workspace. */
export class PathEscapeError extends Error {
  constructor(
    readonly target: string,
    readonly reason: string,
  ) {
    super(`refusing workspace path "${target}": ${reason}`);
    this.name = 'PathEscapeError';
  }
}

function checkRelativeShape(relativePath: string): void {
  if (typeof relativePath !== 'string' || relativePath.length === 0) {
    throw new PathEscapeError(String(relativePath), 'path is empty');
  }
  if (relativePath.includes('\0')) {
    throw new PathEscapeError(relativePath, 'path contains a NUL byte');
  }
  if (
    path.isAbsolute(relativePath)
    || relativePath.startsWith('/')
    || relativePath.startsWith('\\')
    || /^[A-Za-z]:/.test(relativePath)
  ) {
    throw new PathEscapeError(relativePath, 'absolute paths are not allowed; paths must be workspace-relative');
  }
  if (relativePath === '~' || relativePath.startsWith('~/') || relativePath.startsWith('~\\')) {
    throw new PathEscapeError(relativePath, 'home-directory shortcuts (~) are not allowed');
  }
  if (relativePath.split(/[\\/]/).includes('..')) {
    throw new PathEscapeError(relativePath, 'parent traversal ("..") is not allowed');
  }
}

/**
 * True when `target` is `root` itself or lives underneath it. Both arguments
 * should already be fully resolved (realpath'd) paths.
 */
export function isWithinWorkspace(root: string, target: string): boolean {
  const rel = path.relative(path.resolve(root), path.resolve(target));
  if (rel === '') {
    return true;
  }
  if (rel === '..' || rel.startsWith(`..${path.sep}`)) {
    return false;
  }
  return !path.isAbsolute(rel);
}

/** Normalize a workspace-relative path to forward slashes. */
export function normalizeWorkspacePath(relativePath: string): string {
  return relativePath.split('\\').join('/');
}

/**
 * Resolve a workspace-relative path to an absolute path that is guaranteed to
 * stay inside `workspaceRoot` after symlink resolution.
 *
 * Non-existent targets are handled by realpath-ing the deepest existing
 * ancestor and appending the remaining segments, then re-checking containment.
 * Throws {@link PathEscapeError} for any path that could leave the workspace.
 */
export function resolveWorkspacePath(workspaceRoot: string, relativePath: string): string {
  checkRelativeShape(relativePath);

  let rootReal: string;
  try {
    rootReal = realpathSync(workspaceRoot);
  } catch {
    throw new PathEscapeError(workspaceRoot, 'workspace root does not exist or cannot be resolved');
  }

  const segments = relativePath.split(/[\\/]/).filter((segment) => segment.length > 0 && segment !== '.');
  const tail: string[] = [];
  let existing = rootReal;
  for (let depth = segments.length - 1; depth >= 0; depth -= 1) {
    const candidate = path.join(rootReal, ...segments.slice(0, depth + 1));
    if (statSync(candidate, { throwIfNoEntry: false }) !== undefined) {
      existing = candidate;
      break;
    }
    tail.unshift(segments[depth] as string);
  }

  const existingReal = realpathSync(existing);
  if (!isWithinWorkspace(rootReal, existingReal)) {
    throw new PathEscapeError(relativePath, 'resolved location leaves the workspace via a symlink');
  }

  const target = tail.length === 0 ? existingReal : path.join(existingReal, ...tail);
  if (!isWithinWorkspace(rootReal, target)) {
    throw new PathEscapeError(relativePath, 'resolved path leaves the workspace');
  }
  return target;
}
