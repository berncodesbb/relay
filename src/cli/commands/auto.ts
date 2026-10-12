/**
 * `relay auto [bound]` — show or change the autonomy bound.
 */

import {
  NAMED_AUTONOMY_BOUNDS,
  WORKFLOW_MODES,
  parseAutonomyBound,
} from '../../core/modes.js';
import type { WorkflowController } from '../../runtime/controller.js';
import type { OutputWriter } from '../output.js';

export async function runAuto(options: {
  controller: WorkflowController;
  bound: string | undefined;
  output: OutputWriter;
}): Promise<number> {
  const { controller, bound, output } = options;
  if (bound === undefined) {
    output.text(`autonomy: ${controller.state.autonomy}`);
    return 0;
  }
  const parsed = parseAutonomyBound(bound);
  if (parsed === null) {
    output.error(`invalid autonomy bound "${bound}"`);
    output.error(
      `Valid bounds: ${[...NAMED_AUTONOMY_BOUNDS, ...WORKFLOW_MODES.map((mode) => `until:${mode}`)].join(', ')}`,
    );
    return 2;
  }
  const result = await controller.setAutonomy(parsed);
  output.text(`autonomy: ${result.state.autonomy}`);
  output.text(result.outcome.reason);
  return 0;
}
