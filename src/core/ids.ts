import { randomUUID } from 'node:crypto';

/** Fresh identifier for events, artifacts, sessions, and projects. */
export function newId(prefix: string): string {
  return `${prefix}_${randomUUID()}`;
}

export function nowIso(): string {
  return new Date().toISOString();
}
