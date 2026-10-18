import assert from 'node:assert/strict';
import { copyFile, mkdir, readFile, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { buildNativeDispatchers } from '../../tools/build/build-native-dispatchers.mjs';

const execute = promisify(execFile);
const repo = fileURLToPath(new URL('../../', import.meta.url));

/** Current-target consumer composition only; this does not build a universal archive. */
export async function dashboardsPackage(config) {
  const plan = JSON.parse(await readFile(join(repo, 'services/dashboards/deploy.json'), 'utf8'));
  assert.equal(config.version, plan.version);
  const generated = buildNativeDispatchers({ root: repo, component: 'dashboards', outputRoot: join(config.artifactRoot, 'dist') });
  const target = join(config.artifactRoot, 'dist/native', `${process.platform}-${process.arch}`);
  await mkdir(target, { recursive: true });
  const extension = process.platform === 'win32' ? '.exe' : '';
  const service = join(target, 'ivy-dashboards' + extension);
  const health = join(target, 'ivy-health' + extension);
  await copyFile(process.env.DASHBOARDS_TEST_SERVICE, service);
  await copyFile(process.env.DASHBOARDS_TEST_HEALTH, health);
  const stamp = { componentId: plan.componentId, version: plan.version, buildId: config.buildId };
  for (const directory of [join(config.artifactRoot, 'dist'), target]) {
    await writeFile(join(directory, 'build-info.json'), JSON.stringify(stamp));
  }
  await copyFile(join(repo, 'services/dashboards/deploy.json'), join(config.artifactRoot, 'deploy.json'));
  return { main: generated.files[1], health: generated.files[2], service, nativeHealth: health, plan, target };
}

export async function windowsJob(jobName, observed = []) {
  assert.equal(process.platform, 'win32');
  const script = fileURLToPath(new URL('./dashboards-job.ps1', import.meta.url));
  const { stdout } = await execute('powershell.exe', ['-NoProfile', '-NonInteractive', '-File', script,
    '-JobName', jobName, '-ObservedIds', observed.map(row => row.id).join(',')],
  { windowsHide: true, timeout: 15000, maxBuffer: 256 * 1024 });
  return JSON.parse(stdout);
}

export async function packageHealth(entry, config) {
  try {
    const { stdout, stderr } = await execute(process.execPath, [entry, '--config', config],
      { windowsHide: true, timeout: 10000, maxBuffer: 65536 });
    assert.equal(stderr, '');
    return { exitCode: 0, value: JSON.parse(stdout) };
  } catch (error) {
    if (typeof error.code !== 'number') throw error;
    assert.equal(error.stderr, '');
    return { exitCode: error.code, value: JSON.parse(error.stdout) };
  }
}
