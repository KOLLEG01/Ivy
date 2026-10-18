import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { copyFile, mkdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { atomicJson } from '../../../dist/packages/host-runtime/src/config.js';
import { dashboardsPackage, packageHealth, windowsJob } from '../../fixtures/dashboards-package.mjs';

/** Preserve the service fixture's waitReady/close interface for a real native process. */
export async function nativeDashboards(path, client) {
  const config = JSON.parse(await readFile(path, 'utf8'));
  if (process.platform === 'win32') {
    await mkdir(join(config.artifactRoot, 'dist/native'), { recursive: true });
    await copyFile(process.env.DASHBOARDS_TEST_JOB, join(config.artifactRoot, 'dist/native/ivy-job.exe'));
  }
  const packaged = process.env.DASHBOARDS_TEST_DISPATCH ? await dashboardsPackage(config) : null;
  const jobName = packaged && process.platform === 'win32' ? `Global\\Ivy.dashboards-test-${randomUUID()}` : null;
  const executable = jobName ? process.env.DASHBOARDS_TEST_JOB : packaged ? process.execPath : process.env.DASHBOARDS_TEST_SERVICE;
  const args = packaged ? [packaged.main, '--config', path] : ['--config', path];
  if (jobName) args.unshift('--parent', String(process.pid), '--job-name', jobName, process.execPath);
  const launchId = randomUUID();
  let log = '', closeWork, observed;
  const child = spawn(executable, args, {
    windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, IVY_NODE_EXECUTABLE: process.execPath, IVY_LAUNCH_ID: launchId },
  });
  const exited = once(child, 'exit');
  // Readiness and cleanup report the original startup error through the same promise.
  void exited.catch(() => {});
  for (const stream of [child.stdout, child.stderr]) stream.on('data', bytes => { log = (log + bytes).slice(-8000); });
  const alive = () => child.pid && child.exitCode === null && child.signalCode === null;
  const inspect = async () => {
    assert.ok(packaged, 'package mode is required');
    const health = JSON.parse(await readFile(join(config.dataRoot, 'health.json'), 'utf8'));
    assert.notEqual(health.pid, child.pid);
    assert.equal(health.instanceId, config.instanceId);
    assert.equal(health.buildId, config.buildId);
    assert.equal(health.launchId, launchId);
    let job;
    if (jobName) {
      job = await windowsJob(jobName);
      const service = job.processes.find(row => row.id === health.pid);
      assert.equal(service.executable.toLowerCase(), packaged.service.toLowerCase());
      const launcher = job.processes.find(row => row.id === service.parent);
      assert.equal(launcher.executable.toLowerCase(), process.execPath.toLowerCase());
      assert.equal(launcher.parent, child.pid);
      assert.ok(job.members.includes(service.id));
      assert.ok(job.processes.some(row => /chrome-headless-shell\.exe$/i.test(row.executable)));
      observed = job.processes;
    }
    return { health, launcherPid: child.pid, job };
  };
  const retired = async () => {
    if (!observed) return;
    const after = await windowsJob(jobName, observed);
    assert.equal(after.exists, false);
    assert.deepEqual(after.processes.filter(row => observed.some(old => old.id === row.id && old.created === row.created)), []);
  };
  const waitReady = async () => {
    const end = Date.now() + 30000;
    let last;
    while (Date.now() < end) {
      if (!alive()) throw Error('Native Dashboards exited before readiness: ' + log);
      try {
        const node = await client.request('serviceNodes.get', { serviceNodeId: config.serviceNodeId });
        if (node.connected && node.synced && node.ready && node.buildId === config.buildId) {
          if (packaged) {
            const current = await inspect();
            const probe = await packageHealth(packaged.health, path);
            assert.deepEqual(probe, { exitCode: 0, value: { ok: true, buildId: config.buildId, generation: current.health.generation } });
          }
          return;
        }
      } catch (error) { last = error; }
      await delay(50);
    }
    throw Error('Native Dashboards readiness timed out: ' + log, { cause: last });
  };
  const close = () => closeWork ??= (async () => {
    if (!alive()) { await exited; assert.equal(child.exitCode, 0, log); return; }
    const timer = new AbortController();
    try {
      if (packaged) await inspect();
      const health = JSON.parse(await readFile(join(config.dataRoot, 'health.json'), 'utf8'));
      await atomicJson(join(config.dataRoot, 'control.json'), { schemaVersion: 1,
        instanceId: health.instanceId, launchId: health.launchId, bootId: health.bootId,
        action: 'shutdown', requestedAt: new Date().toISOString() });
      await Promise.race([exited, delay(10000, undefined, { signal: timer.signal }).then(() => { throw Error('Native Dashboards shutdown timed out: ' + log); })]);
      assert.equal(child.exitCode, 0, log);
      await retired();
    } finally {
      timer.abort();
      if (alive()) { child.kill(); await exited; }
    }
  })();
  const crash = async () => {
    assert.ok(jobName, 'forced tree proof requires the Windows Job launcher');
    await inspect();
    child.kill();
    await exited;
    await retired();
    closeWork = Promise.resolve();
  };
  return { service: { waitReady }, close, inspect, crash, packaged,
    probe: () => packageHealth(packaged.health, path) };
}
