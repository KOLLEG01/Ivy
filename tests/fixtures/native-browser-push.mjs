import { spawnSync } from 'node:child_process';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { artifactRoot, nativeExecutable, startTestHive, writeJson } from '../../tools/testing/native-service.mjs';

let executable;
function fixtureExecutable() {
  if (executable) return executable;
  const result = spawnSync(process.env.IVY_CARGO ?? 'cargo', ['build', '--locked', '--package', 'ivy-hive',
    '--bin', 'ivy-hive', '--example', 'ivy-hive-browser-fixture', '--message-format=json-render-diagnostics'],
    { encoding: 'utf8', windowsHide: true, maxBuffer: 32 * 1024 * 1024 });
  if (result.error || result.status !== 0) throw result.error ?? Error((result.stderr + result.stdout).slice(-64000));
  const built = result.stdout.split('\n').filter(Boolean).map(line => JSON.parse(line))
    .filter(frame => frame.reason === 'compiler-artifact' && frame.target.name === 'ivy-hive-browser-fixture' && frame.executable);
  if (built.length !== 1) throw Error('Expected the exact native browser Hive fixture executable.');
  return executable = built[0].executable;
}

export async function startBrowserPushHive(options) {
  const executable = fixtureExecutable(), root = join(options.root, 'hive-data');
  await mkdir(root, { recursive: true });
  await writeFile(join(root, 'push-status'), '201');
  const server = await startTestHive({ ...options, executable,
    env: { IVY_TEST_BROWSER_PUSH_ROOT: root, ...(process.platform === 'win32'
      ? { IVY_JOB_LAUNCHER: nativeExecutable(artifactRoot('hive', options.artifact), 'ivy-job') } : {}) } });
  return Object.assign(server, {
    async pushStatus(status) { await writeJson(join(root, 'push-status'), status); },
    async pushDeliveries() {
      const text = await readFile(join(root, 'push-deliveries.jsonl'), 'utf8').catch(error => {
        if (error.code === 'ENOENT') return ''; throw error;
      });
      return text.trim().split('\n').filter(Boolean).map(line => JSON.parse(line));
    },
  });
}
