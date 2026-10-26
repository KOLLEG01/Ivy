import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { randomUUID } from 'node:crypto';
import { resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { startTestHive } from './native-service.mjs';

const { values, positionals } = parseArgs({ allowPositionals: true, options: {
  cwd: { type: 'string' }, 'hive-artifact': { type: 'string' }, 'timeout-ms': { type: 'string' },
} });
if (!values.cwd || !positionals.length) throw Error('Use --cwd CONSUMER_DIRECTORY -- NODE_ARGUMENTS.');
const timeoutMs = Number(values['timeout-ms'] ?? '120000');
if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1000 || timeoutMs > 7200000) throw Error('Invalid harness timeout.');
const root = await mkdtemp(resolve(tmpdir(), 'ivy-consumer-hive-'));
const credential = randomUUID();
let server, child, timer, timedOut = false;
const stopChild = () => {
  if (!child?.pid || child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === 'win32') spawnSync('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, timeout: 15000, stdio: 'ignore' });
  else child.kill('SIGKILL');
};
try {
  server = await startTestHive({ root, artifact: values['hive-artifact'], credentials: [{ principalId: 'isolated-consumer-test', token: credential }] });
  child = spawn(process.execPath, positionals, { cwd: resolve(values.cwd), windowsHide: true, stdio: 'inherit',
    env: { ...process.env, IVY_TEST_HIVE_URL: server.config.publicBaseUrl, IVY_TEST_HIVE_CREDENTIAL: credential,
      IVY_TEST_HIVE_VERSION: server.config.version, IVY_TEST_EVIDENCE: root } });
  timer = setTimeout(() => { timedOut = true; stopChild(); }, timeoutMs);
  const [code, signal] = await once(child, 'exit');
  if (timedOut || signal) console.error('FAIL isolated Hive test process: ' + (timedOut ? 'timeout' : signal));
  process.exitCode = timedOut ? 1 : code ?? 1;
} finally {
  clearTimeout(timer); stopChild();
  try { await server?.close(); } finally { await rm(root, { recursive: true, force: true }); }
}
