/**
 * Transcript: the scrolling conversation and activity log.
 *
 * Completed entries render once through Ink's `<Static>`; only the newest
 * entry (the one a streaming delta may still be mutating) stays in the live
 * region. Entries use stable textual prefixes instead of color so every line
 * is identifiable without color support.
 */

import type { ReactElement } from 'react';
import { Box, Static, Text } from 'ink';
import type { TranscriptEntry } from '../view-model.js';

export type TranscriptProps = {
  readonly entries: readonly TranscriptEntry[];
};

export function entryPrefix(entry: TranscriptEntry): string {
  switch (entry.kind) {
    case 'user':
      return 'you> ';
    case 'assistant':
      return 'relay> ';
    case 'tool':
      return entry.phase === 'start' ? 'tool+ ' : entry.ok === false ? 'tool✘ ' : entry.ok === true ? 'tool✔ ' : 'tool· ';
    case 'artifact':
      return 'art ';
    case 'mode':
      return 'mode ';
    case 'notice':
      return 'note ';
    case 'error':
      return '!err ';
    case 'outcome':
      return 'out ';
  }
}

export function entryText(entry: TranscriptEntry): string {
  switch (entry.kind) {
    case 'user':
      return entry.text;
    case 'assistant':
      return entry.text;
    case 'tool':
      if (entry.phase === 'start') return entry.tool;
      return entry.summary ? `${entry.tool} — ${entry.summary}` : entry.tool;
    case 'artifact':
      return `${entry.artifactKind} v${entry.version} ${entry.status} (${entry.artifactId})`;
    case 'mode': {
      const from = entry.previous ?? 'start';
      return `${from} -> ${entry.mode}${entry.reason ? ` — ${entry.reason}` : ''}`;
    }
    case 'notice':
      return entry.message;
    case 'error':
      return entry.message;
    case 'outcome':
      return `${entry.label} — ${entry.reason}`;
  }
}

function TranscriptRow({ entry }: { readonly entry: TranscriptEntry }): ReactElement {
  const isDim = entry.kind === 'tool' || entry.kind === 'notice' || entry.kind === 'artifact';
  const line = `${entryPrefix(entry)}${entryText(entry)}`;
  // Wrap long lines rather than truncating so evidence stays visible.
  return isDim ? <Text dimColor>{line}</Text> : <Text>{line}</Text>;
}

export function Transcript({ entries }: TranscriptProps): ReactElement {
  const live = entries.at(-1) ?? null;
  const completed = entries.length > 0 ? entries.slice(0, -1) : [];
  return (
    <Box flexDirection="column">
      <Static items={completed}>
        {(item, index) => <TranscriptRow key={index} entry={item} />}
      </Static>
      {live !== null ? <TranscriptRow entry={live} /> : <Text dimColor>(no activity yet)</Text>}
    </Box>
  );
}
