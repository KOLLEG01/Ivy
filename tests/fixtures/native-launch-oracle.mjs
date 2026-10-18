// Actual original launch path, with pinned synthetic executables and loopback peers.
import { readFileSync, writeFileSync, mkdirSync, copyFileSync, existsSync, realpathSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { WebSocketServer } from 'ws';
import { startCodexProcess } from '../../dist/packages/host-runtime/src/codex-process.js';
import { processStopFence } from '../../dist/packages/host-runtime/src/process-fence.js';

const repo = join(dirname(fileURLToPath(import.meta.url)), '../..');
let bytes = '';
for await (const chunk of process.stdin) bytes += chunk;
const input = JSON.parse(bytes);
const { root, variant } = input;
const hash = path => 'sha256:' + createHash('sha256').update(readFileSync(path)).digest('hex');
const json = (path, value) => writeFileSync(path, JSON.stringify(value));
const normalize = value => JSON.parse(JSON.stringify(value).split(JSON.stringify(root).slice(1, -1)).join('<root>'));
const fence = { schemaVersion: 1, owner: 'native', ownerId: 'fixture-epoch', processId: 1234, observedAt: '2026-01-02T03:04:05.000Z', code: 'outcome_unknown' };

function prepare(port = input.port ?? 1) {
  const artifactRoot = join(root, 'artifact');
  const dataRoot = join(root, 'data');
  const home = join(root, 'home');
  const bin = join(root, 'bin');
  const marker = join(root, 'started');
  for (const path of [bin, home, dataRoot, join(artifactRoot, 'dist/native'), join(artifactRoot, 'dist/specs/native/codex-0.154.0')]) mkdirSync(path, { recursive: true });
  const executable = join(bin, process.platform === 'win32' ? 'codex.exe' : 'codex');
  const shell = join(bin, 'pwsh.exe');
  copyFileSync(input.fixture, executable);
  copyFileSync(input.fixture, shell);
  if (process.platform === 'win32') copyFileSync(input.job, join(artifactRoot, 'dist/native/ivy-job.exe'));
  const socket = join(root, 'control.sock');
  writeFileSync(socket, String(port));
  const managed = join(home, 'packages/standalone/current/bin', process.platform === 'win32' ? 'codex.exe' : 'codex');
  mkdirSync(dirname(managed), { recursive: true });
  copyFileSync(input.fixture, managed);
  const daemon = { status: 'started', socketPath: socket, managedCodexPath: managed, pid: 1234, cliVersion: '0.154.0', managedCodexVersion: '0.154.0', appServerVersion: variant === 'daemon-version' ? '0.153.0' : '0.154.0' };
  const mode = variant === 'handshake-version' ? 'launch-wrong-version' : 'launch-duplex';
  const reportedHome = variant === 'handshake-home' ? root : home;
  writeFileSync(join(bin, 'launch-fixture.txt'), [process.execPath, join(repo, 'tests/fixtures/native-connection-child.mjs'), mode, reportedHome, marker, JSON.stringify(daemon), variant === 'cli-version' ? '0.153.0' : '0.154.0', ''].join('\n'));
  const settings = { nativeExecutable: executable, nativeExecutableHash: hash(executable), nativeVersion: '0.154.0', codexHome: home, appServer: { mode: 'owned-stdio' }, windowsShell: { executable: shell, executableHash: hash(shell) } };
  if (['proxy', 'socket-relative'].includes(variant)) settings.appServer = { mode: 'external-proxy', socketPath: variant === 'socket-relative' ? 'control.sock' : socket, expectedCodexHome: home };
  if (['automatic', 'daemon-version', 'expected-home'].includes(variant)) settings.appServer = { mode: 'external-proxy', ...(variant === 'expected-home' ? { expectedCodexHome: root } : {}) };
  if (variant === 'default') delete settings.appServer;
  if (variant === 'claude') settings.appServer = { mode: 'claude-adapter' };
  if (variant === 'relative-executable') settings.nativeExecutable = 'codex.exe';
  if (variant === 'shell-missing') delete settings.windowsShell;
  if (variant === 'shell-hash') settings.windowsShell.executableHash = 'sha256:' + '0'.repeat(64);
  const catalog = JSON.parse(readFileSync(join(repo, 'specs/native/codex-0.154.0/catalog.json'), 'utf8'));
  catalog.nativeExecutableHash = settings.nativeExecutableHash;
  if (variant === 'catalog-hash') catalog.nativeExecutableHash = 'sha256:' + '0'.repeat(64);
  if (variant === 'initial-items') catalog.clientRequests.find(v => v.method === 'thread/realtime/start').inputSchema = { type: 'object' };
  if (variant === 'executable-hash') { settings.nativeExecutableHash = 'sha256:' + '0'.repeat(64); catalog.nativeExecutableHash = settings.nativeExecutableHash; }
  json(join(artifactRoot, 'dist/specs/native/codex-0.154.0/catalog.json'), catalog);
  if (variant === 'fence') { json(join(dataRoot, 'process-stop-fence.json'), fence); rmSync(join(artifactRoot, 'dist/specs/native/codex-0.154.0/catalog.json')); }
  if (variant === 'overlap') settings.codexHome = artifactRoot;
  return { settings, artifactRoot, dataRoot, home, marker, catalog };
}
function observations(prepared) {
  const lines = path => existsSync(path) ? readFileSync(path, 'utf8').trim().split('\n').filter(Boolean).map(line => JSON.parse(line)) : [];
  const frames = lines(prepared.marker + '.frames').map(({ id, ...frame }) => frame.method ? frame : { id, ...frame });
  return { launches: lines(prepared.marker + '.launches'), frames };
}

if (input.action === 'prepare') {
  console.log(JSON.stringify(prepare()));
} else if (input.action === 'observe') {
  const observed = observations({ marker: join(root, 'started') });
  if (input.frames) observed.frames = input.frames;
  console.log(JSON.stringify(normalize({ ...input.result, ...observed })));
} else if (input.action === 'fence') {
  console.log(JSON.stringify(await processStopFence(join(root, 'data'))));
} else {
  let server, peers;
  let port = 1;
  let prepared;
  const trace = [];
  let resolveRequest;
  const requested = new Promise(resolve => { resolveRequest = resolve; });
  const proxy = ['proxy', 'automatic', 'daemon-version'].includes(variant) || variant === 'default' && process.platform === 'win32';
  if (proxy) {
    server = createServer();
    peers = new WebSocketServer({ server, perMessageDeflate: false });
    server.listen(0, '127.0.0.1');
    await once(server, 'listening');
    port = server.address().port;
    peers.on('connection', peer => {
      peer.on('message', data => {
        const frame = JSON.parse(data.toString());
        if (prepared) {
          const log = prepared.marker + '.frames';
          // Keep both transport traces in the same shape as the owned child.
          writeFileSync(log, (existsSync(log) ? readFileSync(log) : '') + JSON.stringify(frame) + '\n');
        }
        if (frame.method === 'initialize') peer.send(JSON.stringify({ id: frame.id, result: { codexHome: prepared.home, userAgent: 'codex/0.154.0' } }));
        else if (frame.method === 'initialized') {
          peer.send(JSON.stringify({ id: 7, method: 'currentTime/read', params: { threadId: 'voice-thread' } }));
          peer.send(JSON.stringify({ method: 'serverRequest/resolved', params: { requestId: 7, threadId: 'voice-thread' } }));
        } else if (frame.method) peer.send(JSON.stringify({ id: frame.id, result: { data: [], nextCursor: null } }));
      });
      peer.on('close', (code, reason) => trace.push({ transportClose: `${code}:${reason}` }));
    });
  }
  prepared = prepare(port);
  let result;
  try {
    const connection = await startCodexProcess({ ...prepared, clientName: 'IvyVoiceFixture', clientVersion: '1.0.0', hiveCredential: 'fixture-credential', onRequest: request => { trace.push({ request }); resolveRequest(request); }, onNotification: notification => trace.push({ notification }), onClose: close => trace.push({ close }) });
    const request = await Promise.race([requested, new Promise((_, reject) => { const timer = setTimeout(() => reject(Error('No server request')), 5000); timer.unref(); })]);
    const reply = await connection.rpc.request('model/list', { limit: 1 }, { requestId: 'voice-models' }, 5000);
    connection.rpc.answer('currentTime/read', request.id, { result: {} }, () => trace.push({ answerIntent: true }));
    await connection.rpc.request('model/list', { limit: 1 }, { requestId: 'after-answer' }, 5000);
    const catalogEqual = JSON.stringify(connection.rpc.catalog) === JSON.stringify(prepared.catalog);
    await Promise.all([connection.close(), connection.close()]);
    const stopped = await connection.completion;
    if (stopped.errorCode === 'outcome_unknown') throw Error('Synthetic owner stop is unknown');
    result = { success: true, mode: connection.mode, home: connection.home, catalogEqual, reply, connected: connection.rpc.connected, fence: await processStopFence(prepared.dataRoot) };
  } catch (error) {
    if (typeof error.toWire !== 'function') throw error;
    result = { success: false, error: error.toWire() };
  } finally {
    if (peers) { await new Promise(resolve => peers.close(resolve)); await new Promise(resolve => server.close(resolve)); }
  }
  console.log(JSON.stringify(normalize({ ...result, trace, ...observations(prepared) })));
}
