/**
 * Status panel and the shared bounded text block used by all focused panels.
 *
 * Panels render pre-formatted text (produced by the renderers in
 * `src/tui/app.tsx`) inside a bounded box. Long content is capped with an
 * explicit marker pointing at `relay inspect` rather than scrolling, so panel
 * height never takes over the terminal.
 */

import type { ReactElement } from 'react';
import { Box, Text } from 'ink';

/** Maximum rendered lines per panel before the truncation marker. */
export const MAX_PANEL_LINES = 14;

export type PanelFrameProps = {
  readonly title: string;
  readonly content: string;
  readonly columns: number;
};

/** Caps rendered lines, replacing the overflow with an explicit marker. */
export function capLines(lines: readonly string[], max: number): string[] {
  if (lines.length <= max) return [...lines];
  const hidden = lines.length - max;
  return [...lines.slice(0, max), `… (${hidden} more line${hidden === 1 ? '' : 's'}, see relay inspect)`];
}

export function PanelFrame({ title, content, columns }: PanelFrameProps): ReactElement {
  const lines = capLines(content.split('\n'), MAX_PANEL_LINES);
  const width = Math.min(Math.max(columns - 2, 20), 120);
  return (
    <Box flexDirection="column" borderStyle="round" paddingLeft={1} paddingRight={1} width={width}>
      <Text bold>{` ${title} `}</Text>
      {lines.map((line, index) => (
        <Text key={index}>{line.length > 0 ? line : ' '}</Text>
      ))}
    </Box>
  );
}

export type StatusPanelProps = {
  readonly content: string;
  readonly columns: number;
};

export function StatusPanel({ content, columns }: StatusPanelProps): ReactElement {
  return <PanelFrame title="status" content={content} columns={columns} />;
}
