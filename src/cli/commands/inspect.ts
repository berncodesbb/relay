/**
 * `relay inspect <artifact>` — print a stored artifact's Markdown body.
 */

import type { WorkflowController } from '../../runtime/controller.js';
import type { OutputWriter } from '../output.js';

export async function runInspect(options: {
  controller: WorkflowController;
  target: string;
  output: OutputWriter;
  json: boolean;
}): Promise<number> {
  const { controller, target, output, json } = options;
  const file = await controller.inspect(target);
  if (file === null) {
    output.error(`artifact not found: ${target}`);
    output.error('Run relay status to list stored artifacts.');
    return 1;
  }
  if (json) {
    output.json({ frontmatter: file.frontmatter, path: file.path });
    return 0;
  }
  output.text(`# ${file.path}`);
  output.text('');
  output.text(file.body.trim().length > 0 ? file.body.trimEnd() : '(empty artifact body)');
  return 0;
}
