import { parseArgs } from 'node:util';
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync, createWriteStream, readFileSync, existsSync, statSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { once } from 'node:events';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import { buildIdentity } from '../../../packages/host-runtime/src/build-identity.mjs';

// A bounded log wrapper around existing runners, not a separate test framework.
const { values, positionals } = parseArgs({ allowPositionals: true, options: Object.fromEntries(
  ['cwd', 'evidence', 'name', 'timeout-ms', 'command'].map(key => [key, { type: 'string' }])) });
for (const key of ['cwd', 'evidence', 'name', 'timeout-ms', 'command']) if (!values[key]) throw new Error('Missing --' + key);
if (!/^[a-z0-9][a-z0-9-]{0,90}$/.test(values.name)) throw new Error('Use a stable filename-safe stage name.');
const timeoutMs = Number(values['timeout-ms']);
if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1000 || timeoutMs > 7200000) throw new Error('Stage timeout must be 1 second to 2 hours.');
const cwd = resolve(values.cwd), root = join(resolve(values.evidence), values.name);
mkdirSync(root); // Refuse to overwrite original evidence or silently repeat a stage.
const reportPath = join(root, 'report.json'), stdout = createWriteStream(join(root, 'stdout.log'), { flags: 'wx' }), stderr = createWriteStream(join(root, 'stderr.log'), { flags: 'wx' });
const runners = [fileURLToPath(import.meta.url), ...positionals.filter(arg => /\.[mc]?js$/.test(arg)).map(arg => resolve(cwd, arg))]
  .filter(path => existsSync(path) && statSync(path).isFile()).map(path => ({ path, hash: 'sha256:' + createHash('sha256').update(readFileSync(path)).digest('hex') }));
const report = { schemaVersion: 1, name: values.name, cwd, command: values.command, args: positionals,
  startedAt: new Date().toISOString(), timeoutMs, runners, build: buildIdentity(cwd), phase: 'running', pid: null };
const save = () => writeFileSync(reportPath, JSON.stringify(report, null, 2) + '\n'); save();
const started = performance.now();
const temporaryRoot = join(root, 'tmp'); mkdirSync(temporaryRoot);
const child = spawn(values.command, positionals, { cwd, env: { ...process.env, TEMP: temporaryRoot, TMP: temporaryRoot, TMPDIR: temporaryRoot }, windowsHide: true, shell: false, stdio: ['ignore', 'pipe', 'pipe'] });
report.pid = child.pid ?? null; save();
child.stdout.pipe(stdout); child.stderr.pipe(stderr);
let failure, timedOut = false;
child.once('error', error => { failure = error.message; });
const timer = setTimeout(() => {
  timedOut = true;
  if (child.pid && child.exitCode === null) {
    // This PID is the exact child just launched by this stage; never select by process name.
    if (process.platform === 'win32') spawnSync('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, timeout: 15000, stdio: 'ignore' });
    else child.kill('SIGKILL');
  }
}, timeoutMs);
const [code, signal] = await new Promise(resolve => child.once('close', (code, signal) => resolve([code, signal])));
clearTimeout(timer);
await Promise.all([stdout.writableFinished ? undefined : once(stdout, 'finish'), stderr.writableFinished ? undefined : once(stderr, 'finish')]);
Object.assign(report, { finishedAt: new Date().toISOString(), durationMs: Math.round(performance.now() - started),
  exitCode: code, signal, timedOut, error: failure ?? null, phase: timedOut ? 'timed_out' : code === 0 && !failure ? 'passed' : 'failed' });
save(); console.log(JSON.stringify(report)); process.exitCode = report.phase === 'passed' ? 0 : 1;
