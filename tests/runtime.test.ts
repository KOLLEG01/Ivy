import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, readFile, writeFile, readdir } from 'node:fs/promises';
import { join, resolve, relative } from 'node:path';
import { tmpdir } from 'node:os';
import { setTimeout as delay } from 'node:timers/promises';
import { createServer } from 'node:net';
import { spawn } from 'node:child_process';
import type { Host } from '../packages/contracts/src/generated.js';
import { HiveClient, IvyError } from '../packages/sdk/src/client.js';
import { startHive } from '../services/hive/src/main.js';
import { atomicJson } from '../packages/host-runtime/src/config.js';
import { checkHealth } from '../packages/host-runtime/src/health.js';
import { startProcess, runCommand, runtimeEnvironment } from '../packages/host-runtime/src/process.js';

const artifactRoot = resolve('.'), executables = { node: process.execPath };
test('a process which writes before exit 1 reports possible partial effects, while rejected launch has none', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-process-outcome-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const marker = join(root, 'effect.txt');
  await assert.rejects(runCommand({ executable: 'node', args: ['-e', 'require("node:fs").writeFileSync(process.argv[1], "effect"); process.exit(1)', marker], timeoutMs: 5000 }, artifactRoot, executables),
    (error: unknown) => error instanceof IvyError && error.code === 'command_failed' && error.outcome === 'unknown' && (error.details as { exitCode: number }).exitCode === 1);
  assert.equal(await readFile(marker, 'utf8'), 'effect');
  await assert.rejects(runCommand({ executable: 'node', cwd: '../', args: [], timeoutMs: 5000 }, artifactRoot, executables), { code: 'invalid_arguments', outcome: 'not_executed' });
});
async function fixture(t: TestContext) {
  const root = await mkdtemp(join(tmpdir(), 'ivy-runtime-test-'));
  const cleanups: (() => Promise<unknown>)[] = [];
  t.after(async () => { for (const cleanup of cleanups.reverse()) await cleanup(); assert.ok(relative(tmpdir(), root).startsWith('ivy-runtime-test-')); await rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 }); });
  const reservation = createServer(); await new Promise<void>(resolve => reservation.listen(0, '127.0.0.1', resolve));
  const port = (reservation.address() as { port: number }).port; await new Promise<void>(resolve => reservation.close(() => resolve()));
  const identity = JSON.parse(await readFile(join(artifactRoot, 'dist', 'build-info.json'), 'utf8')) as { buildId: string; version: string };
  const config: Host.InstanceConfig = { schemaVersion: 1, instanceId: 'fixture', componentId: 'hive', serviceNodeId: 'hive-fixture', hostId: 'isolated-host', ...identity,
    artifactRoot, dataRoot: join(root, 'data'), publicBaseUrl: `http://127.0.0.1:${port}/ivy`, settings: { listenHost: '127.0.0.1', listenPort: port, credentials: [{ principalId: 'fixture', token: 'synthetic-start-token' }], backup: { directory: join(root, 'backups'), intervalHours: 24, retain: 2 } } };
  // Keep the runtime shape independent of any extra provenance fields in build-info.
  const { buildId, version } = identity; config.buildId = buildId; config.version = version;
  for (const key of Object.keys(config)) if (!['schemaVersion', 'hostId', 'instanceId', 'serviceNodeId', 'componentId', 'publicBaseUrl', 'dataRoot', 'artifactRoot', 'buildId', 'version', 'settings'].includes(key)) delete (config as Record<string, unknown>)[key];
  const path = join(root, 'config.json'); await atomicJson(path, config);
  return { root, config, path, cleanups };
}
async function until(check: () => boolean | Promise<boolean>, timeout = 12_000) {
  const end = Date.now() + timeout;
  while (!(await check())) { assert.ok(Date.now() < end, 'runtime condition deadline'); await delay(50); }
}
test('standalone Hive config starts, rotates credentials, verifies health and retains safe snapshots', { timeout: 30_000 }, async t => {
  const { root, config, path, cleanups } = await fixture(t), hive = await startHive(path);
  cleanups.push(() => hive.close());
  const client = new HiveClient(config.publicBaseUrl, { credential: 'synthetic-start-token' });
  await until(async () => { try { return (await checkHealth(config)).ready; } catch { return false; } });
  assert.equal((await client.request('system.status', {})).buildId, config.buildId);
  await hive.backup(); await hive.backup(); await hive.backup();
  assert.equal((await readdir(join(root, 'backups'))).filter(name => name.endsWith('.sqlite')).length, 2);
  const credentials = (config.settings as unknown as Host.HiveSettings).credentials;
  credentials[0]!.token = 'rotated-synthetic-token'; await atomicJson(path, config);
  const rotated = new HiveClient(config.publicBaseUrl, { credential: 'rotated-synthetic-token' });
  await until(async () => { try { return (await rotated.request('system.status', {})).ready; } catch { return false; } });
  await assert.rejects(client.request('system.status', {}), (error: unknown) => error instanceof IvyError && error.code === 'unauthenticated');
  await writeFile(path, '{ invalid configuration');
  await delay(2100); assert.equal((await rotated.request('system.status', {})).ready, true);
});

test('actual Hive executable listens and returns nonzero for unavailable port and invalid configuration', { timeout: 30_000 }, async t => {
  const { root, config, path, cleanups } = await fixture(t);
  const command = { executable: 'node', args: ['dist/services/hive/src/main.js'], timeoutMs: 10000 };
  const process = await startProcess(command, artifactRoot, executables, { environment: { IVY_INSTANCE_CONFIG: path } });
  cleanups.push(() => process.stop());
  const client = new HiveClient(config.publicBaseUrl, { credential: 'synthetic-start-token' });
  await until(async () => { try { return (await client.request('system.status', {}, { timeoutMs: 500 })).ready; } catch { return false; } });
  // Reusing the first database would test its exclusive opener before reaching the occupied port.
  // Keep a finite cold-process budget that includes worker/module startup on the Windows target.
  const contender = join(root, 'port-contender.json');
  await atomicJson(contender, { ...config, instanceId: 'port-contender', dataRoot: join(root, 'contender-data') });
  await assert.rejects(runCommand(command, artifactRoot, executables, { environment: { IVY_INSTANCE_CONFIG: contender } }), (error: unknown) => error instanceof IvyError && error.code === 'command_failed');
  assert.equal((await client.request('system.status', {})).ready, true);
  await assert.rejects(runCommand(command, artifactRoot, executables, { environment: { IVY_INSTANCE_CONFIG: join(config.dataRoot, 'missing.json') } }), (error: unknown) => error instanceof IvyError && error.code === 'command_failed');
});

test('disabled local backups do not create storage or schedule snapshots', { timeout: 15_000 }, async t => {
  const { root, config, path, cleanups } = await fixture(t);
  (config.settings as unknown as Host.HiveSettings).backup.enabled = false;
  await atomicJson(path, config);
  const hive = await startHive(path); cleanups.push(() => hive.close());
  await until(async () => { try { return (await checkHealth(config)).ready; } catch { return false; } });
  await assert.rejects(async () => hive.backup(), { code: 'backup_disabled' });
  assert.ok(!(await readdir(root)).includes('backups'));
});

test('process runner preserves literal arguments, bounds output and deadlines, and excludes ambient secrets', { timeout: 20_000 }, async () => {
  const arguments_ = ['space value', 'quotes"and\\', 'trailing\\', '', '`literal`', '$(literal)'];
  const result = await runCommand({ executable: 'node', args: ['-e', 'process.stdout.write(JSON.stringify(process.argv.slice(1)))', ...arguments_], timeoutMs: 3000 }, artifactRoot, executables);
  assert.deepEqual(JSON.parse(result.stdout), arguments_);
  const output = await runCommand({ executable: 'node', args: ['-e', 'process.stdout.write("a".repeat(200000))'], timeoutMs: 3000 }, artifactRoot, executables);
  assert.equal(output.truncated, true); assert.ok(output.stdout.length <= 65_536);
  const redacted = await runCommand({ executable: 'node', args: ['-e', 'process.stdout.write("some-pass");setTimeout(()=>process.stdout.write("word-value"),30)'], timeoutMs: 3000 }, artifactRoot, executables, { redact: ['password'] });
  assert.equal(redacted.stdout, 'some-[redacted]-value');
  let diagnosticCalls = 0, rawProtocol = '';
  const protocol = await startProcess({ executable: 'node', args: ['-e', 'process.stdout.write("private-native-payload");process.stderr.write("private-native-diagnostic")'], timeoutMs: 3000 }, artifactRoot, executables,
    { captureOutput: false, onOutput: () => { diagnosticCalls++; } });
  protocol.child.stdout!.on('data', (bytes: Buffer) => { rawProtocol += bytes.toString('utf8'); });
  const privateOutput = await protocol.completion;
  assert.equal(rawProtocol, 'private-native-payload'); assert.equal(privateOutput.stdout, ''); assert.equal(privateOutput.stderr, ''); assert.equal(diagnosticCalls, 0);
  await assert.rejects(runCommand({ executable: 'node', args: ['-e', 'setInterval(()=>{},1000)'], timeoutMs: 150 }, artifactRoot, executables), (error: unknown) => error instanceof IvyError && error.code === 'deadline_exceeded');
  process.env['IVY_TEST_AMBIENT_SECRET'] = 'must-not-leak';
  try { assert.equal(runtimeEnvironment()['IVY_TEST_AMBIENT_SECRET'], undefined); }
  finally { delete process.env['IVY_TEST_AMBIENT_SECRET']; }
  const originalPath = Object.entries(process.env).find(([key]) => key.toLowerCase() === 'path');
  assert.ok(originalPath);
  const overrideKey = originalPath[0] === 'PATH' ? 'Path' : 'PATH';
  const projected = runtimeEnvironment({ [overrideKey]: 'isolated-native-shell-path' });
  assert.deepEqual(Object.entries(projected).filter(([key]) => key.toLowerCase() === 'path'), [[overrideKey, 'isolated-native-shell-path']]);
  assert.equal(process.env[originalPath[0]], originalPath[1]);
});

test('runtime environment preserves Windows installation paths without ambient secrets or runtime injection', () => {
  const values = { ProgramData: 'C:\\ProgramData', ALLUSERSPROFILE: 'C:\\ProgramData', ProgramFiles: 'C:\\Program Files', 'ProgramFiles(x86)': 'C:\\Program Files (x86)', IVY_TEST_AMBIENT_SECRET: 'secret', NODE_OPTIONS: '--inspect' };
  const previous = Object.fromEntries(Object.keys(values).map(key => [key, process.env[key]]));
  try {
    Object.assign(process.env, values);
    const projected = runtimeEnvironment();
    for (const [key, value] of Object.entries(values).slice(0, 4)) {
      assert.equal(Object.entries(projected).find(([name]) => name.toLowerCase() === key.toLowerCase())?.[1], value);
    }
    assert.equal(projected['IVY_TEST_AMBIENT_SECRET'], undefined);
    assert.equal(projected['NODE_OPTIONS'], undefined);
    const overridden = runtimeEnvironment({ PROGRAMDATA: 'D:\\isolated-data' });
    assert.deepEqual(Object.entries(overridden).filter(([key]) => key.toLowerCase() === 'programdata'), [['PROGRAMDATA', 'D:\\isolated-data']]);
  } finally {
    for (const [key, value] of Object.entries(previous)) { if (value === undefined) delete process.env[key]; else process.env[key] = value; }
  }
});

test('Windows Job Object kills a real grandchild when its initiating parent exits', { timeout: 20_000, skip: process.platform !== 'win32' }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-runtime-test-'));
  t.after(async () => { assert.ok(relative(tmpdir(), root).startsWith('ivy-runtime-test-')); await rm(root, { recursive: true, force: true }); });
  const pidFile = join(root, 'grandchild.pid');
  const module = new URL('../packages/host-runtime/src/process.js', import.meta.url).href;
  const childScript = 'const cp=require("node:child_process"),fs=require("node:fs");const p=cp.spawn(process.execPath,["-e","setInterval(()=>{},1000)"],{stdio:"ignore"});fs.writeFileSync(process.argv[1],String(p.pid));setInterval(()=>{},1000)';
  const parentScript = `import {startProcess} from ${JSON.stringify(module)}; const child = await startProcess(${JSON.stringify({ executable: 'node', args: ['-e', childScript, pidFile], timeoutMs: 5000 })}, ${JSON.stringify(artifactRoot)}, {node:process.execPath}); setInterval(()=>{},1000);`;
  // This is a plain compiled-module fixture, not another test runner. Inheriting
  // split --test-* values would turn a test-name pattern into its script path.
  const parent = spawn(process.execPath, ['--input-type=module', '-e', parentScript], { windowsHide: true, stdio: ['ignore', 'ignore', 'pipe'] });
  let parentError = '';
  parent.stderr.on('data', (chunk: Buffer) => { parentError = (parentError + chunk.toString('utf8')).slice(-16384); });
  t.after(() => parent.kill());
  let pid = 0;
  await until(async () => {
    assert.equal(parent.exitCode, null, 'Fixture parent exited before creating its grandchild: ' + parentError);
    try { pid = Number(await readFile(pidFile, 'utf8')); return pid > 0; } catch { return false; }
  });
  parent.kill();
  await until(() => { try { process.kill(pid, 0); return false; } catch { return true; } });
});

test('Windows atomic JSON replacement survives a real reader that temporarily denies delete sharing', { timeout: 15_000, skip: process.platform !== 'win32' }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-runtime-test-')), path = join(root, 'state.json'), ready = join(root, 'locked');
  const script = join(root, 'hold.ps1');
  await atomicJson(path, { value: 'previous' });
  await writeFile(script, 'param([string]$DataPath,[string]$ReadyPath)\n$ErrorActionPreference="Stop"\n$stream=[System.IO.File]::Open($DataPath,[System.IO.FileMode]::Open,[System.IO.FileAccess]::Read,[System.IO.FileShare]::Read)\ntry{[System.IO.File]::WriteAllText($ReadyPath,"locked");Start-Sleep -Milliseconds 800}finally{$stream.Dispose()}\n');
  const holder = await startProcess({ executable: 'powershell', args: ['-NoProfile', '-NonInteractive', '-File', script, path, ready], timeoutMs: 5000 }, artifactRoot,
    { powershell: join(process.env['SystemRoot']!, 'System32/WindowsPowerShell/v1.0/powershell.exe') });
  t.after(async () => { await holder.stop(); assert.ok(relative(tmpdir(), root).startsWith('ivy-runtime-test-')); await rm(root, { recursive: true, force: true }); });
  await until(async () => { try { return await readFile(ready, 'utf8') === 'locked'; } catch { return false; } });
  const writing = atomicJson(path, { value: 'next' });
  await delay(50); assert.deepEqual(JSON.parse(await readFile(path, 'utf8')), { value: 'previous' });
  await writing; assert.deepEqual(JSON.parse(await readFile(path, 'utf8')), { value: 'next' });
  assert.equal((await holder.completion).exitCode, 0);
});
