/**
 * `relay status` — goal, mode, autonomy, artifacts, blockers, next action.
 */

import type { WorkflowController } from '../../runtime/controller.js';
import { renderStatus, type OutputWriter } from '../output.js';

export async function runStatus(options: {
  controller: WorkflowController;
  output: OutputWriter;
  json: boolean;
}): Promise<number> {
  const { controller, output, json } = options;
  const view = controller.statusView();
  if (json) {
    output.json(view);
    return 0;
  }
  const rendered = renderStatus(view);
  // The controller state projection has a guaranteed shape, so it floors the
  // output even when the view's own fields evolve.
  const floor = statusFloor(controller, rendered);
  output.text(floor.trim().length > 0 ? floor.trimEnd() : '(no status available)');
  return 0;
}

function statusFloor(controller: WorkflowController, rendered: string): string {
  const lines = rendered.split('\n').filter((line) => line.length > 0);
  const has = (label: string): boolean => lines.some((line) => line.toLowerCase().startsWith(label));
  const state = controller.state;
  const extras: string[] = [];
  if (!has('mode:') && !has('mode ')) extras.push(`Mode: ${state.currentMode}`);
  if (!has('autonomy:')) extras.push(`Autonomy: ${state.autonomy}`);
  if (!has('goal:')) extras.push(`Goal: ${state.goal}`);
  if (extras.length === 0) return lines.join('\n');
  return [...lines, '', ...extras].join('\n');
}
