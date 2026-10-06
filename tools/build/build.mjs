import { compileBackend, stageBackend } from './build-backend.mjs';
import { buildWeb } from './build-web.mjs';
import { rmSync } from 'node:fs';
const identity = compileBackend({ native: true });
await buildWeb();
if (process.platform === 'win32') {
  // The complete local build includes the self-contained Windows voice runtime.
  // Its publisher deliberately requires a fresh destination so stale framework
  // files can never survive a rebuild.
  rmSync('dist/native/phone', { recursive: true, force: true });
  await import('./build-phone-runtime.mjs');
}
stageBackend(identity);
