/**
 * Plain-text output for non-interactive Relay commands.
 *
 * All CLI output flows through an {@link OutputWriter} so commands never call
 * `console.*` and output can be captured or redirected in tests. Writes go to
 * synchronous file descriptors so nothing is lost when the process exits
 * immediately after printing. The status/why renderers are deliberately
 * defensive: view shapes evolve with the controller, so every field is probed
 * with type guards and raw objects are never printed.
 */

import { writeSync } from 'node:fs';
import type { StatusView, WhyView } from '../runtime/controller-views.js';

export type OutputWriter = {
  text(line: string): void;
  json(value: unknown): void;
  error(line: string): void;
};

export function createOutputWriter(options: {
  json: boolean;
  out?: NodeJS.WriteStream;
  err?: NodeJS.WriteStream;
}): OutputWriter {
  const out = options.out ?? process.stdout;
  const err = options.err ?? process.stderr;
  const emit = (stream: NodeJS.WriteStream, text: string): void => {
    const payload = `${text}\n`;
    const fd = (stream as unknown as { fd?: number }).fd;
    try {
      if (typeof fd === 'number') {
        writeSync(fd, payload);
        return;
      }
      stream.write(payload);
    } catch {
      // Some stream kinds reject synchronous writes; fall back to buffered.
      stream.write(payload);
    }
  };
  return {
    // Plain text is suppressed in --json mode so machine output stays parseable.
    text(line: string): void {
      if (options.json) return;
      emit(out, line);
    },
    json(value: unknown): void {
      emit(out, JSON.stringify(value, null, 2));
    },
    error(line: string): void {
      emit(err, line);
    },
  };
}

// -- defensive view rendering -------------------------------------------------

type Loose = Record<string, unknown>;

function asLoose(value: unknown): Loose {
  return typeof value === 'object' && value !== null ? (value as Loose) : {};
}

function isLoose(value: unknown): value is Loose {
  return typeof value === 'object' && value !== null;
}

function stringOf(value: unknown): string | null {
  return typeof value === 'string' && value.trim().length > 0 ? value : null;
}

function firstString(source: Loose, keys: readonly string[]): string | null {
  for (const key of keys) {
    const found = stringOf(source[key]);
    if (found !== null) return found;
  }
  return null;
}

function stringArray(value: unknown): readonly string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((entry): entry is string => typeof entry === 'string' && entry.length > 0);
}

function bullet(text: string): string {
  return `  - ${text}`;
}

/** Render a status view as aligned key: value blocks plus artifact/blocker lists. */
export function renderStatus(view: StatusView): string {
  const v = asLoose(view);
  const lines: string[] = [];

  const goal = firstString(v, ['goal']);
  if (goal !== null) lines.push(`Goal: ${goal}`);

  const mode = firstString(v, ['mode', 'currentMode', 'stage']);
  if (mode !== null) lines.push(`Mode: ${mode}`);

  const autonomy = firstString(v, ['autonomy', 'autonomyBound', 'bound']);
  if (autonomy !== null) lines.push(`Autonomy: ${autonomy}`);

  const projectId = firstString(v, ['projectId', 'project']);
  if (projectId !== null) lines.push(`Project: ${projectId}`);

  if (v.paused !== null && v.paused !== undefined) {
    const paused = asLoose(v.paused);
    const reason = firstString(paused, ['reason']) ?? 'paused';
    lines.push(`Paused: ${reason}`);
    const question = firstString(paused, ['question']);
    if (question !== null) lines.push(`  question: ${question}`);
    for (const option of stringArray(paused.options)) {
      lines.push(`  option: ${option}`);
    }
  }

  const progress = describeProgress(v.progress);
  if (progress !== null) lines.push(`Progress: ${progress}`);

  const active = firstString(v, ['activeWork', 'active', 'activity']);
  if (active !== null) lines.push(`Active: ${active}`);

  const stale = stringArray(v.staleArtifacts);
  if (stale.length > 0) {
    lines.push('Stale artifacts:');
    for (const artifactId of stale) lines.push(`! ${artifactId} is stale`);
  }

  const artifacts = collectArtifacts(v.artifacts);
  if (artifacts.length > 0) {
    lines.push('Artifacts:');
    lines.push(...renderArtifactRows(artifacts));
  }

  const blockers = collectBlockers(v.blockers ?? v.pauseBlockers);
  if (blockers.length > 0) {
    lines.push('Blockers:');
    for (const blocker of blockers) lines.push(`! ${blocker}`);
  }

  const changed = stringArray(v.changedFiles);
  if (changed.length > 0) {
    lines.push('Changed files:');
    for (const file of changed) lines.push(`  ${file}`);
  }

  const stats = asLoose(v.stats);
  const statsParts: string[] = [];
  const events = typeof stats.events === 'number' ? stats.events : null;
  if (events !== null) statsParts.push(`events: ${events}`);
  const modelCalls = typeof stats.modelCalls === 'number' ? stats.modelCalls : null;
  if (modelCalls !== null) statsParts.push(`model calls: ${modelCalls}`);
  const toolCalls = typeof stats.toolCalls === 'number' ? stats.toolCalls : null;
  if (toolCalls !== null) statsParts.push(`tool calls: ${toolCalls}`);
  if (statsParts.length > 0) lines.push(`Stats: ${statsParts.join('  ')}`);

  const next = firstString(v, ['nextAction', 'next', 'suggestedAction']);
  if (next !== null) lines.push(`Next: ${next}`);

  return lines.join('\n');
}

/** Render an explanation view: mode, last transition, evidence, routing, pause. */
export function renderWhy(view: WhyView): string {
  const v = asLoose(view);
  const lines: string[] = [];

  const mode = firstString(v, ['mode', 'currentMode']);
  if (mode !== null) lines.push(`Mode: ${mode}`);

  const lastOutcome = asLoose(v.lastOutcome);
  const outcomeText = firstString(lastOutcome, ['outcome']);
  const outcomeReason = firstString(lastOutcome, ['reason']);
  if (outcomeText !== null || outcomeReason !== null) {
    lines.push(`Last outcome: ${outcomeText ?? ''}${outcomeReason !== null ? ` — ${outcomeReason}` : ''}`.trimEnd());
  }

  const transition = latestTransition(v);
  if (transition !== null) lines.push(`Last transition: ${transition}`);

  const evidence = stringArray(
    lastOutcome.evidence !== undefined ? lastOutcome.evidence : v.evidence,
  );
  if (evidence.length > 0) {
    lines.push('Evidence:');
    for (const item of evidence) lines.push(bullet(item));
  }

  const routingLines = renderRouting(v.routing ?? v.routes);
  if (routingLines.length > 0) {
    lines.push('Routing:');
    lines.push(...routingLines);
  }

  const rejected = collectRejections(v.rejectedTransitions);
  if (rejected.length > 0) {
    lines.push('Rejected transitions:');
    for (const item of rejected) lines.push(bullet(item));
  }

  const invalidated = stringArray(v.staleArtifacts ?? v.invalidatedArtifacts);
  if (invalidated.length > 0) {
    lines.push('Invalidated artifacts:');
    for (const item of invalidated) lines.push(bullet(item));
  }

  const pausedReason = firstString(v, ['pausedReason']) ?? firstString(asLoose(v.paused), ['reason']);
  if (pausedReason !== null) lines.push(`Paused: ${pausedReason}`);

  const explanation = firstString(v, ['explanation', 'summary', 'detail']);
  if (explanation !== null) lines.push(explanation);

  return lines.join('\n');
}

// -- internals ----------------------------------------------------------------

function describeProgress(value: unknown): string | null {
  if (stringOf(value) !== null) return stringOf(value);
  if (!isLoose(value)) return null;
  const record = value;
  const total = typeof record.total === 'number' ? record.total : null;
  const stage = typeof record.stage === 'number' ? record.stage : null;
  const completed = stringArray(record.completedModes ?? record.completed);
  if (total !== null && stage !== null) return `stage ${stage}/${total}`;
  if (total !== null && completed.length > 0) return `${completed.length}/${total} modes (${completed.join(', ')})`;
  if (completed.length > 0) return completed.join(', ');
  return null;
}

/** Latest entry of a transitions array rendered as `from -> to (source) — reason`. */
function latestTransition(v: Loose): string | null {
  const direct = asLoose(v.lastTransition ?? v.transition);
  const fromList = Array.isArray(v.recentTransitions) ? [...v.recentTransitions] : [];
  const entry = fromList.at(-1);
  const record = entry !== undefined ? asLoose(entry) : direct;
  const from = firstString(record, ['from', 'fromMode']);
  const to = firstString(record, ['to', 'toMode']);
  const source = firstString(record, ['source']);
  const reason = firstString(record, ['reason']);
  if (from === null && to === null && reason === null) return null;
  const path = from !== null && to !== null ? `${from} -> ${to}` : (to ?? from ?? '');
  const qualifier = source !== null ? ` (${source})` : '';
  const why = reason !== null ? ` — ${reason}` : '';
  return `${path}${qualifier}${why}`;
}

function collectRejections(value: unknown): readonly string[] {
  if (!Array.isArray(value)) return [];
  const lines: string[] = [];
  for (const entry of value.slice(-5)) {
    const record = asLoose(entry);
    const from = firstString(record, ['from', 'fromMode']);
    const to = firstString(record, ['to', 'toMode']);
    const reason = firstString(record, ['reason']);
    if (from === null || to === null) continue;
    lines.push(`${from} -> ${to} rejected${reason !== null ? ` — ${reason}` : ''}`);
  }
  return lines;
}

type ArtifactRow = {
  readonly kind: string | null;
  readonly version: string | null;
  readonly status: string | null;
  readonly title: string | null;
};

function collectArtifacts(value: unknown): readonly ArtifactRow[] {
  const rows: ArtifactRow[] = [];
  const push = (entry: unknown): void => {
    const record = asLoose(entry);
    const row: ArtifactRow = {
      kind: firstString(record, ['kind']),
      version: typeof record.version === 'number' ? `v${record.version}` : firstString(record, ['versionLabel']),
      status: firstString(record, ['status']),
      title: firstString(record, ['title', 'name', 'id']),
    };
    if (row.kind !== null || row.title !== null) rows.push(row);
  };
  if (Array.isArray(value)) {
    for (const entry of value) push(entry);
    return rows;
  }
  if (isLoose(value)) {
    for (const entry of Object.values(value)) push(entry);
  }
  return rows;
}

function renderArtifactRows(rows: readonly ArtifactRow[]): readonly string[] {
  const cells = rows.map((row) => [
    row.kind ?? 'artifact',
    row.version ?? '',
    row.status ?? '',
    row.title ?? '',
  ]);
  const widths = [0, 1, 2].map((index) =>
    Math.max(...cells.map((cell) => (cell[index] ?? '').length), 0),
  );
  return cells.map(
    (cell) =>
      `  ${(cell[0] ?? '').padEnd(widths[0] ?? 0)}  ${(cell[1] ?? '').padEnd(widths[1] ?? 0)}  ${(cell[2] ?? '').padEnd(widths[2] ?? 0)}  ${cell[3] ?? ''}`.trimEnd(),
  );
}

function collectBlockers(value: unknown): readonly string[] {
  if (Array.isArray(value)) {
    const blockers: string[] = [];
    for (const entry of value) {
      const text = stringOf(entry) ?? firstString(asLoose(entry), ['message', 'reason', 'question', 'text']);
      if (text !== null) blockers.push(text);
    }
    return blockers;
  }
  const text = stringOf(value);
  return text !== null ? [text] : [];
}

function renderRouting(value: unknown): readonly string[] {
  const lines: string[] = [];
  const describeRoute = (record: Loose): string | null => {
    const provider = firstString(record, ['provider', 'providerName']);
    const model = firstString(record, ['model']);
    if (provider !== null && model !== null) return `${provider}/${model}`;
    return provider ?? model;
  };
  if (Array.isArray(value)) {
    for (const entry of value.slice(-12)) {
      const record = asLoose(entry);
      const mode = firstString(record, ['mode', 'purpose', 'key']);
      const route = describeRoute(record);
      const failed = record.ok === false;
      const prefix = failed ? '! ' : '  ';
      if (mode !== null || route !== null) lines.push(`${prefix}${mode ?? 'route'}: ${route ?? ''}`.trimEnd());
    }
    return lines;
  }
  if (isLoose(value)) {
    for (const [key, entry] of Object.entries(value)) {
      const route = stringOf(entry) ?? (isLoose(entry) ? describeRoute(entry) : null);
      if (route !== null) lines.push(`  ${key}: ${route}`);
    }
  }
  return lines;
}
