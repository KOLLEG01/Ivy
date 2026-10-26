import { cpSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { resolve } from 'node:path';

/** Intentional public Node SDK and CLI launchers; services are native binaries. */
export function buildSdk({ example = false } = {}) {
  let project = 'tsconfig.runtime.json';
  if (example) {
    mkdirSync('.local/build', { recursive: true });
    project = '.local/build/sdk-example.tsconfig.json';
    writeFileSync(project, JSON.stringify({ extends: resolve('tsconfig.runtime.json'), include: [],
      files: [resolve('docs/examples/services/automation-example/src/main.ts')] }));
  }
  const result = spawnSync(process.execPath, ['node_modules/typescript/bin/tsc', '-p', project],
    { stdio: 'inherit', windowsHide: true, timeout: 120000 });
  if (result.error || result.status !== 0) throw result.error ?? Error('SDK compilation failed.');
  stageSdk();
}

export function stageSdk() {
  rmSync('dist/specs', { recursive: true, force: true });
  for (const path of ['specs/schemas', 'specs/native', 'instructions']) cpSync(path, 'dist/' + path, { recursive: true });
  mkdirSync('dist/packages/host-runtime/src', { recursive: true });
  for (const name of ['build-identity.mjs', 'native-plan.mjs']) cpSync('packages/host-runtime/src/' + name, 'dist/packages/host-runtime/src/' + name);
}

if (import.meta.main) buildSdk();
