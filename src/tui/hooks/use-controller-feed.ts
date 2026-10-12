/**
 * Controller feed hook: bridges the workflow controller into React.
 *
 * Owns one view model instance, forwards every controller UI event into it,
 * keeps a controller state snapshot for header/pause rendering, and forces a
 * re-render on each dispatch so components always read a fresh snapshot.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import type { ProjectState } from '../../core/project-state.js';
import {
  createViewModel,
  readControllerState,
  type ControllerUiEvent,
  type TuiAction,
  type TuiController,
  type TuiViewModel,
  type ViewModelHandle,
} from '../view-model.js';

export type ControllerFeed = {
  readonly vm: TuiViewModel;
  readonly dispatch: (action: TuiAction) => void;
  readonly snapshot: ProjectState;
  /** Re-reads the synchronous controller state (call after operations). */
  readonly refresh: () => void;
};

export function useControllerFeed(controller: TuiController): ControllerFeed {
  const viewModelRef = useRef<ViewModelHandle | null>(null);
  let viewModel = viewModelRef.current;
  if (viewModel === null) {
    viewModel = createViewModel();
    viewModelRef.current = viewModel;
  }
  const [, setTick] = useState(0);
  const [snapshot, setSnapshot] = useState<ProjectState>(() => readControllerState(controller));

  const dispatch = useCallback((action: TuiAction): void => {
    viewModelRef.current?.dispatch(action);
    setTick((tick) => tick + 1);
  }, []);

  const refresh = useCallback((): void => {
    setSnapshot(readControllerState(controller));
  }, [controller]);

  useEffect(() => {
    const handleEvent = (event: ControllerUiEvent): void => {
      viewModelRef.current?.dispatch({ type: 'controller-event', event });
      setTick((tick) => tick + 1);
      setSnapshot(readControllerState(controller));
    };
    const unsubscribe = controller.subscribe(handleEvent);
    return () => {
      unsubscribe();
    };
  }, [controller]);

  return {
    vm: viewModel.getState(),
    dispatch,
    snapshot,
    refresh,
  };
}
