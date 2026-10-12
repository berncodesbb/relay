/**
 * `relay pause [reason]` — pause automatic advancement.
 */

import type { WorkflowController } from '../../runtime/controller.js';
import type { OutputWriter } from '../output.js';

export async function runPause(options: {
  controller: WorkflowController;
  reason: string | undefined;
  output: OutputWriter;
}): Promise<number> {
  const { controller, reason, output } = options;
  const trimmed = reason?.trim() ?? '';
  const result = await controller.pause(trimmed.length > 0 ? trimmed : 'paused by user');
  output.text(`Paused: ${result.state.paused?.reason ?? 'paused'}`);
  if (result.state.paused?.question !== null && result.state.paused?.question !== undefined) {
    output.text(`  question: ${result.state.paused.question}`);
  }
  output.text(`Mode: ${result.state.currentMode}`);
  output.text('Resume with: relay run');
  return 0;
}
