import { spawn, spawnSync } from 'node:child_process';
import { createServer } from 'node:net';
import { once } from 'node:events';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { randomUUID } from 'node:crypto';
import { resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { fileURLToPath } from 'node:url';
import { HiveServer } from '../../dist/services/hive/src/server.js';
import { digest } from '../../dist/packages/contracts/src/canonical.js';

// Explicit acceptance harness: the public build never locates or imports a consumer checkout.
const { values, positionals } = parseArgs({ allowPositionals: true, options: { cwd: { type: 'string' }, 'chat-fixture': { type: 'boolean' }, 'timeout-ms': { type: 'string' } } });
if (!values.cwd || !positionals.length) throw new Error('Use --cwd CONSUMER_DIRECTORY -- NODE_ARGUMENTS.');
const timeoutMs = Number(values['timeout-ms'] ?? '120000');
if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1000 || timeoutMs > 7200000) throw new Error('Harness timeout must be an integer from 1000 to 7200000 ms.');
const project = fileURLToPath(new URL('../../', import.meta.url));
const root = await mkdtemp(resolve(tmpdir(), 'ivy-consumer-hive-'));
const reservation = createServer(); reservation.listen(0, '127.0.0.1'); await once(reservation, 'listening');
const port = reservation.address().port; await new Promise((yes, no) => reservation.close(error => error ? no(error) : yes()));
const base = `http://127.0.0.1:${port}/ivy`, credential = randomUUID();
const server = new HiveServer({ filename: resolve(root, 'hive.sqlite'), publicBaseUrl: base,
  version: 'isolated-consumer-test', buildId: digest('isolated-consumer-test'),
  credentials: [{ principalId: 'isolated-consumer-test', digest: digest(credential) }],
  listenHost: '127.0.0.1', listenPort: port, consoleRoot: resolve(project, 'dist/console') });
let child, timer, timedOut = false;
const stopChild = () => {
  if (!child?.pid || child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === 'win32') spawnSync('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, timeout: 15000, stdio: 'ignore' });
  else child.kill();
};
try {
  await server.start();
  child = spawn(process.execPath, positionals, { cwd: resolve(values.cwd), windowsHide: true, stdio: 'inherit',
    env: { ...process.env, IVY_TEST_HIVE_URL: base, IVY_TEST_HIVE_CREDENTIAL: credential, IVY_TEST_EVIDENCE: root,
      ...(values['chat-fixture'] ? { IVY_TEST_CHAT_FIXTURE: resolve(project, 'tools/testing/test-chat-fixture.mjs') } : {}) } });
  timer = setTimeout(() => { timedOut = true; stopChild(); }, timeoutMs);
  const [code, signal] = await once(child, 'exit');
  if (timedOut || signal) console.error(`FAIL isolated Hive test process: ${timedOut ? 'timeout' : signal}`);
  process.exitCode = timedOut ? 1 : code ?? 1;
} finally { clearTimeout(timer); stopChild(); await server.close(); await rm(root, { recursive: true, force: true }); }
