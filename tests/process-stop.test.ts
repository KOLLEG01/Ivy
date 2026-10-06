import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { access, mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { join, resolve, relative } from 'node:path';
import { tmpdir } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';
import { startProcess, runCommand } from '../packages/host-runtime/src/process.js';
import { processStopFence, recordProcessStopFence, requireClearProcessStopFence } from '../packages/host-runtime/src/process-fence.js';
import { startNativeProcess } from '../services/agent-manager/src/process.js';
import { reconcilePhoneProcessStopFence } from '../services/phone-bridge/src/runtime/process.js';
import { digest } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import { until } from './fixtures/host.js';

async function escapedPipe(t: TestContext) {
  const root = await mkdtemp(join(tmpdir(), 'ivy-stop-test-')), pidPath = join(root, 'pid.json'), helper = join(root, 'detached-helper.cjs');
  await writeFile(helper, 'require("node:fs").writeFileSync(process.argv[2],JSON.stringify({pid:process.pid}));setTimeout(()=>{},20000);');
  t.after(async () => {
    const pid = await readFile(pidPath, 'utf8').then(text => (JSON.parse(text) as { pid: number }).pid, () => null);
    if (pid) {
      // Only the exact test helper may be cleaned up; a reused PID is never signalled.
      const command = await readFile('/proc/' + pid + '/cmdline', 'utf8').catch(() => '');
      if (command.split('\0').includes(helper)) process.kill(pid, 'SIGKILL');
    }
    assert.ok(relative(tmpdir(), root).startsWith('ivy-stop-test-')); await rm(root, { recursive: true, force: true });
  });
  const code = 'const cp=require("node:child_process");const child=cp.spawn(process.execPath,[process.argv[1],process.argv[2]],{detached:true,stdio:"inherit"});child.unref();process.stdout.write("root-exit\\n");';
  return { root, pidPath, command: { executable: 'node', args: ['-e', code, helper, pidPath], timeoutMs: 3000 } };
}

test('actual escaped Linux pipe yields one finite unknown stop even after the root exited', { skip: process.platform !== 'linux', timeout: 15_000 }, async t => {
  const f = await escapedPipe(t), running = await startProcess(f.command, resolve('.'), { node: process.execPath });
  t.after(() => running.stop());
  await until(async () => running.child.exitCode === 0 && !!await readFile(f.pidPath, 'utf8').catch(() => ''), 5000);
  const pid = (JSON.parse(await readFile(f.pidPath, 'utf8')) as { pid: number }).pid;
  const began = Date.now(), first = running.stop(100), second = running.stop(100);
  assert.equal(first, second); const result = await first;
  assert.ok(Date.now() - began < 6000); assert.equal(result.errorCode, 'outcome_unknown'); assert.equal(result.exitCode, 0);
  assert.equal(result.truncated, true); assert.equal(result.stdout, 'root-exit\n');
  assert.doesNotThrow(() => process.kill(pid, 0), 'No clean tree termination may be claimed.');
  await delay(100); assert.equal(await running.completion, result); assert.equal(await running.stop(0), result);
});

test('runCommand reports a bounded unknown timeout while an escaped Linux process holds the pipe', { skip: process.platform !== 'linux', timeout: 15_000 }, async t => {
  const f = await escapedPipe(t);
  const began = Date.now();
  await assert.rejects(runCommand(f.command, resolve('.'), { node: process.execPath }),
    (error: unknown) => error instanceof IvyError && error.code === 'deadline_exceeded' && error.outcome === 'unknown');
  assert.ok(Date.now() - began < 9000);
});

test('retained stop evidence fences a new native owner before any epoch or executable launch', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-stop-test-'));
  t.after(async () => { assert.ok(relative(tmpdir(), root).startsWith('ivy-stop-test-')); await rm(root, { recursive: true, force: true }); });
  await requireClearProcessStopFence(root);
  const fence = { schemaVersion: 1 as const, owner: 'native' as const, ownerId: 'previous-native-epoch', processId: null,
    observedAt: new Date().toISOString(), code: 'outcome_unknown' as const };
  assert.equal(await recordProcessStopFence(root, fence), true);
  assert.equal(await recordProcessStopFence(root, { ...fence, ownerId: 'later-observation' }), false);
  assert.deepEqual(JSON.parse(await readFile(join(root, 'process-stop-fence.json'), 'utf8')), fence);
  let beganEpoch = false;
  await assert.rejects(startNativeProcess({ artifactRoot: resolve('.'), dataRoot: root, clientVersion: 'test',
    settings: { nativeExecutable: process.execPath, nativeHome: join(root, 'native'), nativeVersion: '0.154.0', nativeExecutableHash: digest('not launched'), limits: { maxOperations: 100, maxJournalBytes: 64 * 1024 * 1024, maxPendingInputs: 16, maxNotificationBytes: 1048576 } },
    beginEpoch: () => { beganEpoch = true; }, onClose: () => {}, onRequest: () => {}, onNotification: () => {} }),
  (error: unknown) => error instanceof IvyError && error.code === 'outcome_unknown' && error.outcome === 'unknown');
  assert.equal(beganEpoch, false);
  await writeFile(join(root, 'process-stop-fence.json'), '{ incomplete fence');
  await assert.rejects(requireClearProcessStopFence(root), (error: unknown) => error instanceof IvyError && error.code === 'outcome_unknown');
});

test('Phone fence is cleared only after the exact native lifetime lock is released', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-phone-fence-test-'));
  t.after(async () => { assert.ok(relative(tmpdir(), root).startsWith('ivy-phone-fence-test-')); await rm(root, { recursive: true, force: true }); });
  const fence = { schemaVersion: 1 as const, owner: 'native' as const, ownerId: '71b7f513-c61c-42bb-afc0-aa1907f01d01', processId: 123,
    observedAt: new Date().toISOString(), code: 'outcome_unknown' as const };
  await recordProcessStopFence(root, fence);
  await writeFile(join(root, 'phone-native.lock'), '');
  await writeFile(join(root, 'phone-native-owner.json'), JSON.stringify({ schemaVersion: 1, epoch: fence.ownerId, pid: 456, creationFileTime: '123456789' }));
  assert.equal(await reconcilePhoneProcessStopFence(root, async () => false), false);
  assert.deepEqual(await processStopFence(root), fence);
  assert.equal(await reconcilePhoneProcessStopFence(root, async () => true), true);
  assert.equal(await processStopFence(root), null);
});

test('native executable bytes must match the retained catalog before creating a home or beginning an epoch', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-stop-test-')), nativeHome = join(root, 'native');
  t.after(async () => { assert.ok(relative(tmpdir(), root).startsWith('ivy-stop-test-')); await rm(root, { recursive: true, force: true }); });
  const catalog = JSON.parse(await readFile('specs/native/codex-0.154.0/catalog.json', 'utf8')) as { nativeExecutableHash: string };
  let beganEpoch = false;
  // Node is a real large executable, but it is not the public Codex binary named by this catalog.
  await assert.rejects(startNativeProcess({ artifactRoot: resolve('.'), dataRoot: root, clientVersion: 'test',
    settings: { nativeExecutable: process.execPath, nativeHome, nativeVersion: '0.154.0', nativeExecutableHash: catalog.nativeExecutableHash, limits: { maxOperations: 100, maxJournalBytes: 64 * 1024 * 1024, maxPendingInputs: 16, maxNotificationBytes: 1048576 } },
    beginEpoch: () => { beganEpoch = true; }, onClose: () => {}, onRequest: () => {}, onNotification: () => {} }),
    (error: unknown) => error instanceof IvyError && error.code === 'native_build_mismatch' && error.message.includes('bytes differ'));
  assert.equal(beganEpoch, false);
  await assert.rejects(access(nativeHome), (error: unknown) => (error as NodeJS.ErrnoException).code === 'ENOENT');
});
