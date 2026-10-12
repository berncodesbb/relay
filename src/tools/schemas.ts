/**
 * Shared Zod argument-schema helpers for tool definitions.
 *
 * Tools compose these builders so every model-supplied argument is validated
 * the same way before a tool body runs.
 */

import path from 'node:path';
import { z } from 'zod';

function isSafeRelativePath(value: string): boolean {
  return (
    !value.includes('\0')
    && !path.isAbsolute(value)
    && !value.startsWith('/')
    && !value.startsWith('\\')
    && !/^[A-Za-z]:/.test(value)
    && value !== '~'
    && !value.startsWith('~/')
    && !value.startsWith('~\\')
    && !value.split(/[\\/]/).includes('..')
  );
}

/** Workspace-relative path: non-empty, bounded, no absolutes, "~", or "..". */
export const relativePathArg = z
  .string()
  .min(1)
  .max(1000)
  .refine(isSafeRelativePath, {
    message:
      'path must be workspace-relative: not absolute, no "~" shortcut, no drive letter, no NUL byte, and no ".." segments',
  });

/** A non-empty string bounded to `max` characters. */
export function boundedString(max: number, min = 1): z.ZodString {
  return z.string().min(min).max(max);
}

/** Reusable argument schemas shared by several tools. */
export const commonArgs = {
  /** Workspace-relative file or directory path. */
  path: relativePathArg.describe('Workspace-relative path (never absolute, never containing "..").'),
  /** Bounded timeout in milliseconds; run-command clamps it to settings. */
  timeoutMs: z.number().int().min(100).max(600_000),
} as const;
