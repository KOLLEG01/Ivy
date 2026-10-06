import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile, readdir, realpath, copyFile, cp } from 'node:fs/promises';
import { resolve, join, sep, dirname, basename } from 'node:path';
import { parseArgs } from 'node:util';
import { setTimeout as delay } from 'node:timers/promises';
import { startNativeProcess } from '../../../dist/services/agent-manager/src/process.js';
import { startProcess } from '../../../dist/packages/host-runtime/src/process.js';
import { fileHash } from '../../../dist/packages/host-runtime/src/artifact.js';
import { atomicJson } from '../../../dist/packages/host-runtime/src/config.js';

// No model turns, login, desktop discovery or user-home access. Every server is a fresh test child.
const { values } = parseArgs({ options: { settings: { type: 'string' }, root: { type: 'string' } } });
assert.ok(values.settings && values.root);
const root = resolve(values.root); assert.ok(root.startsWith(resolve('.local') + sep));
await mkdir(root); const home = join(root, 'home'), adapterHome = join(root, 'adapter-home');
await mkdir(home); await mkdir(adapterHome);
const input = JSON.parse(await readFile(values.settings, 'utf8'));
const settings = input.settings ?? input;
assert.equal(await fileHash(settings.nativeExecutable), settings.nativeExecutableHash);
const common = { nativeExecutable: settings.nativeExecutable, nativeExecutableHash: settings.nativeExecutableHash, nativeVersion: settings.nativeVersion, limits: { maxOperations: 100, maxJournalBytes: 67108864, maxPendingInputs: 16, maxNotificationBytes: 1048576 },
  ...(settings.windowsShell ? { windowsShell: settings.windowsShell } : {}) };
// Minimal protocol-fixture artifacts are siblings of the test homes, never their owner.
const artifactRoot = join(root, 'artifact'), jobLauncher = join(artifactRoot, 'dist/native/ivy-job.exe');
await mkdir(join(artifactRoot, 'dist/specs/native'), { recursive: true });
await cp(resolve(`dist/specs/native/codex-${settings.nativeVersion}`), join(artifactRoot, `dist/specs/native/codex-${settings.nativeVersion}`), { recursive: true });
const artifacts = process.platform === 'win32' ? ['dist/native/ivy-job.exe'] : [];
for (const name of artifacts) {
  const source = resolve(name), target = join(artifactRoot, name);
  await mkdir(dirname(target), { recursive: true }); await copyFile(source, target);
  assert.equal(await fileHash(target), await fileHash(source));
}
const socket = join(root, 'server.sock'); assert.ok(Buffer.byteLength(socket) < 100, 'Use a short .local root for the Unix socket.');
const report = { schemaVersion: 1, startedAt: new Date().toISOString(), phase: 'running', nativeVersion: settings.nativeVersion,
  executableHash: settings.nativeExecutableHash, root, cases: [], modelTurns: 0 };
let server, connection;
const connect = async (overrides, suffix) => startNativeProcess({ artifactRoot, dataRoot: join(root, suffix), settings: { ...common, ...overrides },
  clientVersion: 'isolated-connection-test', beginEpoch: () => {}, onClose: () => {}, onRequest: () => {}, onNotification: () => {} });
try {
  await writeFile(join(home, 'user-marker.txt'), 'Preserve shared home');
  connection = await connect({ codexHome: home, appServer: { mode: 'owned-stdio' } }, 'owned-manager');
  assert.equal(connection.connectionMode, 'owned-stdio'); assert.equal(connection.ownsNativeHome, false);
  assert.equal(connection.nativeHome, await realpath(home)); await connection.close(); connection = null;
  assert.equal(await readFile(join(home, 'user-marker.txt'), 'utf8'), 'Preserve shared home');
  report.cases.push('owned-stdio-preserves-explicit-shared-home');
  server = await startProcess({ executable: 'codex', args: ['ui-server', '--listen', 'unix://' + socket.replaceAll('\\', '/')], timeoutMs: 30000 }, home,
    { codex: settings.nativeExecutable }, { jobLauncher, environment: { CODEX_HOME: home } });
  const deadline = Date.now() + 15000;
  // Windows AF_UNIX rendezvous entries can reject lstat with EACCES. Enumeration
  // detects their arrival; only the subsequent real protocol handshake proves readiness.
  while (!(await readdir(dirname(socket))).includes(basename(socket))) {
    assert.equal(server.child.exitCode, null, 'Explicit isolated Unix listener exited before connecting.');
    assert.ok(Date.now() < deadline, 'Explicit isolated Unix socket did not appear.'); await delay(100);
  }
  const external = { codexHome: adapterHome, appServer: { mode: 'external-proxy', socketPath: socket, expectedCodexHome: home } };
  connection = await connect(external, 'proxy-manager');
  assert.equal(connection.connectionMode, 'external-proxy'); assert.equal(connection.ownsNativeHome, false);
  assert.equal(connection.nativeHome, await realpath(home)); await connection.close(); connection = null;
  assert.equal(server.child.exitCode, null); process.kill(server.child.pid, 0);
  report.cases.push('proxy-close-leaves-external-server-running-and-reports-server-home');
  await assert.rejects(connect({ ...external, appServer: { ...external.appServer, expectedCodexHome: adapterHome } }, 'mismatch-manager'),
    error => error.code === 'native_home_mismatch');
  assert.equal(server.child.exitCode, null); process.kill(server.child.pid, 0);
  connection = await connect(external, 'reconnect-manager'); await connection.close(); connection = null;
  assert.equal(server.child.exitCode, null); report.cases.push('home-mismatch-closes-only-proxy-and-reconnection-succeeds');
  assert.equal(await readFile(join(home, 'user-marker.txt'), 'utf8'), 'Preserve shared home');
  report.phase = 'passed';
} catch (error) {
  report.phase = 'failed'; report.error = { code: error.code ?? error.name, message: error.message }; process.exitCode = 1;
} finally {
  await connection?.close();
  if (server) {
    const result = await server.stop(); report.serverCleanup = { exitCode: result.exitCode, errorCode: result.errorCode };
    if (result.errorCode === 'outcome_unknown') { report.phase = 'failed'; process.exitCode = 1; }
    // Diagnostic stream belongs only to the fresh test server, whose home has no user credentials.
    await writeFile(join(root, 'server-stderr.log'), result.stderr);
    await writeFile(join(root, 'server-stdout.log'), result.stdout);
  }
  report.finishedAt = new Date().toISOString(); await atomicJson(join(root, 'report.json'), report);
  console.log(JSON.stringify(report));
}
