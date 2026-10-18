import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { atomicJson } from '../../../dist/packages/host-runtime/src/config.js';

/** Preserve the service fixture's waitReady/close interface for a real native process. */
export async function nativeDashboards(path, client) {
  const config = JSON.parse(await readFile(path, 'utf8'));
  if (process.platform === 'win32') {
    await mkdir(join(config.artifactRoot, 'dist/native'), { recursive: true });
    await copyFile(process.env.DASHBOARDS_TEST_JOB, join(config.artifactRoot, 'dist/native/ivy-job.exe'));
  }
  let log = '', closeWork;
  const child = spawn(process.env.DASHBOARDS_TEST_SERVICE, ['--config', path], {
    windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, IVY_NODE_EXECUTABLE: process.execPath, IVY_LAUNCH_ID: randomUUID() },
  });
  const exited = once(child, 'exit');
  // Readiness and cleanup report the original startup error through the same promise.
  void exited.catch(() => {});
  for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => { log = (log + bytes).slice(-8000); });
  const alive = () => child.pid && child.exitCode === null && child.signalCode === null;
  const waitReady = async () => {
    const end = Date.now() + 30000;
    let last;
    while (Date.now() < end) {
      if (!alive()) throw Error('Native Dashboards exited before readiness: ' + log);
      try {
        const node = await client.request('serviceNodes.get', { serviceNodeId: config.serviceNodeId });
        if (node.connected && node.synced && node.ready && node.buildId === config.buildId) return;
      } catch (error) { last = error; }
      await delay(50);
    }
    throw Error('Native Dashboards readiness timed out: ' + log, { cause: last });
  };
  const close = () => closeWork ??= (async () => {
    if (!alive()) { await exited; assert.equal(child.exitCode, 0, log); return; }
    const timer = new AbortController();
    try {
      const health = JSON.parse(await readFile(join(config.dataRoot, 'health.json'), 'utf8'));
      await atomicJson(join(config.dataRoot, 'control.json'), { schemaVersion: 1,
        instanceId: health.instanceId, launchId: health.launchId, bootId: health.bootId,
        action: 'shutdown', requestedAt: new Date().toISOString() });
      await Promise.race([exited, delay(10000, undefined, { signal: timer.signal }).then(() => { throw Error('Native Dashboards shutdown timed out: ' + log); })]);
      assert.equal(child.exitCode, 0, log);
    } finally {
      timer.abort();
      if (alive()) { child.kill(); await exited; }
    }
  })();
  return { service: { waitReady }, close };
}
