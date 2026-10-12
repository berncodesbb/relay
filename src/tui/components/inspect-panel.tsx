/**
 * Inspect panel: the rendered body of one artifact or inspected target.
 */

import type { ReactElement } from 'react';
import { PanelFrame } from './status-panel.js';

export type InspectPanelProps = {
  readonly content: string;
  readonly columns: number;
};

export function InspectPanel({ content, columns }: InspectPanelProps): ReactElement {
  return <PanelFrame title="inspect" content={content} columns={columns} />;
}
