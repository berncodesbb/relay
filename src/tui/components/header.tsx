/**
 * Header: mode, stage, autonomy bound, pause indicator, and the goal line.
 *
 * All indicators are textual (never color-only) so the header stays readable
 * in non-color and screen-reader environments.
 */

import type { ReactElement } from 'react';
import { Box, Text } from 'ink';
import { MODE_CONTRACTS, WORKFLOW_MODES, type WorkflowMode } from '../../core/modes.js';
import type { ProjectState } from '../../core/project-state.js';

export type HeaderProps = {
  readonly snapshot: ProjectState;
  readonly columns: number;
  readonly uninitialized: boolean;
};

const MODE_COUNT = WORKFLOW_MODES.length;

/** Truncates text to `max` columns, appending an ellipsis when it does not fit. */
export function truncateText(text: string, max: number): string {
  const limit = Math.max(4, max);
  if (text.length <= limit) return text;
  return `${text.slice(0, limit - 1)}…`;
}

function modeTitle(mode: string): string {
  return (WORKFLOW_MODES as readonly string[]).includes(mode)
    ? MODE_CONTRACTS[mode as WorkflowMode].title
    : mode;
}

function stageOf(mode: string): number {
  const index = (WORKFLOW_MODES as readonly string[]).indexOf(mode);
  return index === -1 ? 1 : index + 1;
}

export function Header({ snapshot, columns, uninitialized }: HeaderProps): ReactElement {
  const stage = stageOf(snapshot.currentMode);
  const title = modeTitle(snapshot.currentMode);
  const parts = [`relay — ${title} [${stage}/${MODE_COUNT}]`, `auto:${snapshot.autonomy}`];
  if (snapshot.paused) parts.push('!paused');
  const line1 = parts.join(' · ');
  const goal = snapshot.goal.length > 0
    ? snapshot.goal
    : uninitialized
      ? 'no project yet — type a goal to start'
      : '(no goal set)';
  return (
    <Box flexDirection="column">
      <Text bold>{truncateText(line1, columns)}</Text>
      <Text dimColor>{truncateText(goal, Math.max(0, columns - 4))}</Text>
    </Box>
  );
}
