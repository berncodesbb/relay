// Marks the compiled CLI entry executable so `./dist/cli.js` and packed
// tarballs carry the exec bit (npm also forces bin modes on install).
import { chmodSync } from 'node:fs';

chmodSync(new URL('../dist/cli.js', import.meta.url), 0o755);
