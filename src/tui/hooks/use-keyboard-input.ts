/**
 * Keyboard input state machine for the prompt line.
 *
 * Wraps Ink's `useInput` behind a stable subscription and exposes the current
 * value plus submit behavior. Printable input appends (pastes arrive as one
 * string), backspace/delete removes the last character, enter submits, and
 * up/down walk the last {@link HISTORY_LIMIT} submitted inputs. Control and
 * meta combinations are ignored here so app-level shortcuts (ctrl-c) keep
 * working.
 */

import { useCallback, useRef, useState } from 'react';
import { useInput, type Key } from 'ink';

export const HISTORY_LIMIT = 20;

export type KeyboardInputOptions = {
  /** When false, keystrokes are ignored (a controller op is in flight). */
  readonly enabled: boolean;
  /** Called with the raw buffer on enter. */
  readonly onSubmit: (value: string) => void;
};

export type KeyboardInput = {
  readonly value: string;
  readonly setValue: (value: string) => void;
  readonly history: readonly string[];
};

export function useKeyboardInput(options: KeyboardInputOptions): KeyboardInput {
  const [value, setValue] = useState('');
  const historyRef = useRef<string[]>([]);
  const cursorRef = useRef<number>(-1);
  const draftRef = useRef('');

  const optionsRef = useRef(options);
  optionsRef.current = options;
  const valueRef = useRef(value);
  valueRef.current = value;

  const submit = useCallback(() => {
    const current = valueRef.current;
    if (current.length === 0) return;
    const history = historyRef.current;
    history.push(current);
    if (history.length > HISTORY_LIMIT) history.splice(0, history.length - HISTORY_LIMIT);
    cursorRef.current = -1;
    draftRef.current = '';
    setValue('');
    optionsRef.current.onSubmit(current);
  }, []);

  const handler = useCallback(
    (input: string, key: Key): void => {
      if (!optionsRef.current.enabled) return;
      if (key.return) {
        submit();
        return;
      }
      if (key.backspace || key.delete) {
        setValue((previous) => previous.slice(0, -1));
        return;
      }
      if (key.upArrow) {
        const history = historyRef.current;
        if (history.length === 0) return;
        const next = cursorRef.current === -1 ? history.length - 1 : Math.max(0, cursorRef.current - 1);
        if (cursorRef.current === -1) draftRef.current = valueRef.current;
        cursorRef.current = next;
        setValue(history[next] ?? '');
        return;
      }
      if (key.downArrow) {
        const history = historyRef.current;
        if (cursorRef.current === -1) return;
        const next = cursorRef.current + 1;
        if (next >= history.length) {
          cursorRef.current = -1;
          setValue(draftRef.current);
          return;
        }
        cursorRef.current = next;
        setValue(history[next] ?? '');
        return;
      }
      if (key.ctrl || key.meta || key.escape || key.tab) return;
      if (input.length === 0) return;
      setValue((previous) => previous + input);
    },
    [submit],
  );

  useInput(handler);

  return { value, setValue, history: historyRef.current };
}
