/**
 * `relay why` — explain the last routing/transition decision.
 */

import type { WorkflowController } from '../../runtime/controller.js';
import { renderWhy, type OutputWriter } from '../output.js';

export async function runWhy(options: {
  controller: WorkflowController;
  output: OutputWriter;
  json: boolean;
}): Promise<number> {
  const { controller, output, json } = options;
  const view = controller.whyView();
  if (json) {
    output.json(view);
    return 0;
  }
  const rendered = renderWhy(view).trim();
  output.text(rendered.length > 0 ? rendered : '(no decisions recorded yet)');
  return 0;
}
