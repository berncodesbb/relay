/**
 * Prompt line.
 *
 * Composes {@link useKeyboardInput} (Ink `useInput`) with a textual prompt
 * glyph. While a controller operation is in flight the line is disabled and
 * shows `working…` so the state change is visible without color.
 */

import type { ReactElement } from 'react';
import { Text } from 'ink';
import { useKeyboardInput } from '../hooks/use-keyboard-input.js';

export type InputLineProps = {
  readonly busy: boolean;
  readonly onSubmit: (value: string) => void;
};

export function InputLine({ busy, onSubmit }: InputLineProps): ReactElement {
  const { value } = useKeyboardInput({ enabled: !busy, onSubmit });
  if (busy) {
    return <Text>{"› working… (ctrl-c to cancel)"}</Text>;
  }
  return <Text>{`› ${value}`}</Text>;
}
