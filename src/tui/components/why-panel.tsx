/**
 * Why panel: the most recent routing or transition decision with its evidence.
 */

import type { ReactElement } from 'react';
import { PanelFrame } from './status-panel.js';

export type WhyPanelProps = {
  readonly content: string;
  readonly columns: number;
};

export function WhyPanel({ content, columns }: WhyPanelProps): ReactElement {
  return <PanelFrame title="why" content={content} columns={columns} />;
}
