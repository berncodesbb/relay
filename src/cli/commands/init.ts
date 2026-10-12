/**
 * `relay init` — create local Relay state in `.relay/` and nothing else.
 *
 * Re-running init on an initialized workspace is a safe no-op that reports the
 * existing project without writing events or touching any user file.
 */

import type { OutputWriter } from '../output.js';
import { createCliContext } from '../context.js';

export async function runInit(options: {
  workspaceRoot: string;
  goal: string | undefined;
  output: OutputWriter;
}): Promise<number> {
  const { workspaceRoot, goal, output } = options;
  const context = createCliContext({ workspaceRoot });
  try {
    const existing = context.repository.listProjects()[0];
    if (existing !== undefined) {
      output.text(`Relay project already exists (goal: ${existing.goal})`);
      output.text(`State: ${workspaceRoot}/.relay/state.db`);
      output.text('Next: relay run (or relay status)');
      return 0;
    }

    const trimmedGoal = goal?.trim() ?? '';
    const effectiveGoal = trimmedGoal.length > 0 ? trimmedGoal : 'Untitled Relay project';
    const created = await context.repository.initializeProject({
      goal: effectiveGoal,
      workspaceRoot,
      autonomy: context.config.config.autonomy,
    });
    output.text(`Initialized Relay project in ${workspaceRoot}`);
    output.text(`Goal: ${created.state.goal}`);
    output.text(`Next: relay run "${effectiveGoal}"`);
    return 0;
  } finally {
    context.close();
  }
}
