import assert from 'node:assert/strict';
import { cp, mkdir, readFile, writeFile, realpath } from 'node:fs/promises';
import { resolve, join, sep } from 'node:path';
import { parseArgs } from 'node:util';
import { startNativeProcess } from '../../../dist/services/agent-manager/src/process.js';
import { runCommand } from '../../../dist/packages/host-runtime/src/process.js';
import { fileHash } from '../../../dist/packages/host-runtime/src/artifact.js';

// Actual official standalone server in a fresh, credential-free home. No model turn
// and no desktop process. Only this runner's newly installed daemon is stopped.
const { values } = parseArgs({ options: { settings: { type: 'string' }, package: { type: 'string' }, root: { type: 'string' } } });
assert.ok(values.settings && values.package && values.root);
const root = resolve(values.root);
assert.ok(root.startsWith(resolve('.local') + sep));
await mkdir(root);
const input = JSON.parse(await readFile(values.settings, 'utf8')), source = input.settings ?? input;
assert.equal(source.nativeVersion, '0.154.0');
const home = join(root, 'home'), current = join(home, 'packages/standalone/current');
await mkdir(current, { recursive: true });
await cp(resolve(values.package), current, { recursive: true, errorOnExist: true });
const executable = join(current, 'bin', process.platform === 'win32' ? 'codex.exe' : 'codex');
assert.equal(await fileHash(executable), source.nativeExecutableHash);
await writeFile(join(home, 'user-marker'), 'Shared home must survive manager shutdown.');
await writeFile(join(home, 'config.toml'), '[mcp_servers.identity_probe]\ncommand = ' + JSON.stringify(process.execPath.replaceAll('\\', '/')) +
  '\nargs = [' + JSON.stringify(resolve('tests/acceptance/fixtures/native-daemon-mcp-fixture.mjs').replaceAll('\\', '/')) + ']\n' +
  'env_vars = ["IVY_HIVE_TOKEN"]\n[mcp_servers.identity_probe.env]\nIVY_TEST_IDENTITY = "shared-home"\n');
const artifactRoot = join(root, 'artifact'), jobLauncher = join(artifactRoot, 'dist/native/ivy-job.exe');
await mkdir(join(artifactRoot, 'dist/specs/native'), { recursive: true });
await cp(resolve('dist/specs/native/codex-0.154.0'), join(artifactRoot, 'dist/specs/native/codex-0.154.0'), { recursive: true });
if (process.platform === 'win32') {
  await mkdir(join(artifactRoot, 'dist/native'));
  await cp(resolve('dist/native/ivy-job.exe'), jobLauncher);
}
const settings = { nativeExecutable: executable, nativeExecutableHash: source.nativeExecutableHash, nativeVersion: source.nativeVersion,
  codexHome: home, limits: { maxOperations: 100, maxJournalBytes: 67108864, maxPendingInputs: 16, maxNotificationBytes: 1048576 },
  ...(source.windowsShell ? { windowsShell: source.windowsShell } : {}) };
const report = { schemaVersion: 1, startedAt: new Date().toISOString(), nativeVersion: source.nativeVersion, executableHash: source.nativeExecutableHash,
  root, phase: 'running', cases: [], modelTurns: 0 };
const connections = new Set();
const connect = async (name, overrides = {}) => {
  const connection = await startNativeProcess({ artifactRoot, dataRoot: join(root, name), settings: { ...settings, ...overrides },
    // This instance credential must never appear in a shared daemon's environment.
    hiveCredential: 'isolated-manager-credential-not-for-shared-daemon',
    clientVersion: 'daemon-acceptance', beginEpoch() {}, onClose() {}, onRequest() {}, onNotification() {} });
  connections.add(connection); return connection;
};
const close = async connection => { await connection.close(); connections.delete(connection); };
const lifecycle = async action => runCommand({ executable: 'codex', args: ['ui-server', 'daemon', action], timeoutMs: 100000 }, home,
  { codex: executable }, { jobLauncher, environment: { CODEX_HOME: home }, allowWindowsBreakaway: true });
try {
  const outcomes = await Promise.allSettled([connect('manager-a'), connect('manager-b')]);
  for (const outcome of outcomes) if (outcome.status === 'rejected') throw outcome.reason;
  const [first, second] = outcomes.map(outcome => outcome.value);
  assert.equal(first.connectionMode, 'external-proxy'); assert.equal(second.connectionMode, 'external-proxy');
  assert.equal(first.actualServerVersion, '0.154.0'); assert.equal(first.nativeHome, await realpath(home));
  assert.equal(first.socketPath, second.socketPath); assert.equal(first.ownsNativeHome, false);
  assert.deepEqual([first.daemon.status, second.daemon.status].sort(), ['alreadyRunning', 'started']);
  report.daemon = { socketPath: first.socketPath, observations: [first.daemon, second.daemon] };
  report.cases.push('omitted-mode-concurrent-first-start-and-reuse');
  const task = await first.rpc.request('thread/start', { cwd: root, ephemeral: false }, {}, 30000);
  assert.ok('result' in task, 'Real task creation failed');
  const threadId = task.result.thread.id;
  assert.equal(typeof threadId, 'string'); report.threadId = threadId;
  const tool = await first.rpc.request('mcpServer/tool/call', { threadId, server: 'identity_probe', tool: 'identity', arguments: {} }, {}, 30000);
  assert.ok('result' in tool, 'The actual ui server must invoke the configured MCP fixture');
  assert.notEqual(tool.result.isError, true);
  const identity = JSON.parse(tool.result.content.find(item => item.type === 'text').text);
  assert.deepEqual(identity, { sharedIdentity: true, instanceCredentialPresent: false });
  report.cases.push('native-mcp-uses-shared-home-identity-without-manager-credential');
  await close(first);
  const read = await second.rpc.request('thread/read', { threadId, includeTurns: false }, {}, 30000);
  assert.ok('result' in read); assert.equal(read.result.thread.id, threadId);
  await close(second);
  const version = await lifecycle('version'); assert.equal(version.exitCode, 0);
  report.afterManagerStop = JSON.parse(version.stdout);
  report.cases.push('task-shared-across-proxies-and-daemon-survives-both-manager-stops');
  const next = await connect('manager-reconnect');
  assert.equal(next.daemon.status, 'alreadyRunning'); assert.equal(next.socketPath, report.daemon.socketPath);
  await close(next); report.cases.push('reconnect-reuses-original-daemon');
  const wrongHome = join(root, 'wrong-home'); await mkdir(wrongHome);
  await assert.rejects(connect('wrong-home-manager', { appServer: { mode: 'external-proxy', socketPath: report.daemon.socketPath, expectedCodexHome: wrongHome } }),
    error => error.code === 'native_home_mismatch');
  const explicit = await connect('explicit-manager', { appServer: { mode: 'external-proxy', socketPath: report.daemon.socketPath, expectedCodexHome: home } });
  assert.equal(explicit.daemon, null); await close(explicit);
  report.cases.push('explicit-endpoint-is-authoritative-and-wrong-home-is-rejected');
  await assert.rejects(connect('missing-endpoint-manager', { appServer: { mode: 'external-proxy', socketPath: join(root, 'missing.sock') } }));
  const afterMissing = await connect('after-missing-manager'); assert.equal(afterMissing.socketPath, report.daemon.socketPath); await close(afterMissing);
  report.cases.push('missing-explicit-endpoint-fails-without-fallback');
  const ownedHome = join(root, 'owned-home'); await mkdir(ownedHome);
  const owned = await connect('owned-manager', { codexHome: ownedHome, appServer: { mode: 'owned-stdio' } });
  assert.equal(owned.connectionMode, 'owned-stdio'); assert.equal(owned.daemon, null); await close(owned);
  report.cases.push('explicit-owned-stdio-remains-available');
  assert.equal(await readFile(join(home, 'user-marker'), 'utf8'), 'Shared home must survive manager shutdown.');
  report.phase = 'passed';
} catch (error) {
  report.phase = 'failed'; report.error = { code: error.code ?? error.name, message: error.message, stack: error.stack }; process.exitCode = 1;
} finally {
  for (const connection of connections) await close(connection).catch(error => { report.cleanupError = error.message; report.phase = 'failed'; process.exitCode = 1; });
  const stopped = await lifecycle('stop');
  report.cleanup = stopped;
  if (stopped.exitCode !== 0 || stopped.errorCode !== null) { report.phase = 'failed'; process.exitCode = 1; }
  report.finishedAt = new Date().toISOString();
  await writeFile(join(root, 'report.json'), JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify({ phase: report.phase, cases: report.cases, error: report.error, report: join(root, 'report.json') }));
}
