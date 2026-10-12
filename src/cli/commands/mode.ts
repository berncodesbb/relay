/**
 * `relay mode <mode>` — override the active workflow mode.
 *
 * Invalid mode names are usage errors. Transition rejections raised by policy
 * print the policy reason and fail; successful overrides confirm the new mode.
 */

import { WORKFLOW_MODES, isWorkflowMode } from '../../core/modes.js';
import type { WorkflowController } from '../../runtime/controller.js';
import type { OutputWriter } from '../output.js';

export async function runMode(options: {
  controller: WorkflowController;
  mode: string;
  output: OutputWriter;
}): Promise<number> {
  const { controller, mode, output } = options;
  if (!isWorkflowMode(mode)) {
    output.error(`unknown mode "${mode}"`);
    output.error(`Valid modes: ${WORKFLOW_MODES.join(', ')}`);
    return 2;
  }
  try {
    const result = await controller.setMode(mode);
    if (result.outcome.kind !== 'CONTINUE') {
      // setMode returns NEEDS_USER when policy rejects the transition.
      output.error(`transition rejected: ${result.outcome.reason}`);
      output.error(`Workflow remains in mode ${result.state.currentMode}`);
      return 1;
    }
    output.text(`Mode: ${result.state.currentMode}`);
    output.text(result.outcome.reason);
    return 0;
  } catch (error) {
    if (error instanceof Error && error.name === 'PolicyRejectionError') {
      output.error(error.message);
      return 1;
    }
    throw error;
  }
}
