import { describe, expect, it } from 'vitest';
import { RELAY_DIR, relayPaths } from '../../../src/storage/paths.js';

describe('relayPaths', () => {
  it('derives all Relay state paths under <root>/.relay', () => {
    const paths = relayPaths('/w/my-project');
    expect(paths.root).toBe(`/w/my-project/${RELAY_DIR}`);
    expect(paths.database).toBe(`/w/my-project/${RELAY_DIR}/state.db`);
    expect(paths.artifactsDir).toBe(`/w/my-project/${RELAY_DIR}/artifacts`);
  });

  it('exposes the relay directory name', () => {
    expect(RELAY_DIR).toBe('.relay');
  });
});
