import test from 'node:test';
import { resolve } from 'node:path';
import { runCommand } from '../packages/host-runtime/src/process.js';

test('Phone native PCM, codec timing and local SIP lifecycle preserve media gates without devices',
  { skip: process.platform !== 'win32', timeout: 390_000 }, async t => {
    // The native checker already asserts every required marker and enforces the exit status.
    await runCommand({ executable: 'node', args: [resolve('tools/operations/check-phone-runtime.mjs')], timeoutMs: 370_000 },
      resolve('.'), { node: process.execPath },
      { onOutput: (_stream, text) => { t.diagnostic(text.slice(-8000)); },
        environment: Object.fromEntries(['IVY_DOTNET', 'IVY_EVS_CC', 'IVY_EVS_JOBS', 'IVY_EVS_ARCHIVE', 'IVY_EVS_BUILD_REPORT']
        .flatMap(name => process.env[name] ? [[name, process.env[name]!]] : [])) });
  });
