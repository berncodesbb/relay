/**
 * Pause overlay: shown whenever the workflow is paused for a user decision.
 *
 * The overlay states why relay stopped, asks the focused question, lists the
 * options as a numbered list, and offers a small set of concrete commands the
 * user can type next. Everything is textual; no meaning is carried by color.
 */

import type { ReactElement } from 'react';
import { Box, Text } from 'ink';

export type PauseOverlayProps = {
  /** Why relay stopped. */
  readonly reason: string;
  /** Focused question to answer, when the controller provided one. */
  readonly question: string | null;
  readonly options: readonly string[];
  /** True when this pause awaits a decision (vs. a plain user pause). */
  readonly decision: boolean;
};

const DECISION_COMMANDS: readonly (readonly [string, string])[] = [
  ['/approve', 'approve this artifact and continue'],
  ['/revise prd <why>', 'send it back with your reason'],
  ['/auto workflow', 'let relay decide and continue automatically'],
  ['/why', 'inspect the evidence behind this pause'],
];

const PAUSE_COMMANDS: readonly (readonly [string, string])[] = [
  ['/resume', 'continue the workflow'],
  ['/status', 'see where things stand'],
  ['/why', 'inspect the last decision'],
  ['/auto <bound>', 'change the autonomy bound'],
];

function commandWidth(commands: readonly (readonly [string, string])[]): number {
  return commands.reduce((max, [command]) => Math.max(max, command.length), 0);
}

export function PauseOverlay({ reason, question, options, decision }: PauseOverlayProps): ReactElement {
  const commands = decision ? DECISION_COMMANDS : PAUSE_COMMANDS;
  const commandColumn = commandWidth(commands);
  return (
    <Box flexDirection="column" borderStyle="round" paddingLeft={1} paddingRight={1}>
      <Text bold>{decision ? '! relay needs your decision' : '! relay is paused'}</Text>
      {reason.length > 0 ? <Text>{`  reason: ${reason}`}</Text> : null}
      {question !== null && question.length > 0 ? <Text>{`  ${question}`}</Text> : null}
      {options.length > 0
        ? options.map((option, index) => (
          <Text key={`${index}-${option}`}>{`  ${index + 1}) ${option}`}</Text>
        ))
        : null}
      <Text>{'  what you can do:'}</Text>
      {commands.map(([command, description]) => (
        <Text key={command}>{`    ${command.padEnd(commandColumn)}  ${description}`}</Text>
      ))}
      <Text dimColor>{'  type a command or an answer and press enter'}</Text>
    </Box>
  );
}
