import { fingerprint } from '../build/build-cache.mjs';
import { mkdirSync, writeFileSync } from 'node:fs';
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, realpathSync, rmSync, existsSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve, relative, isAbsolute } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { selectTests, dependencies, implicitSelectionTooLarge } from './test-selection.mjs';
import * as config from './test-config.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');
const { values, positionals } = parseArgs({ allowPositionals: true, options: {
  changed: { type: 'boolean' }, since: { type: 'string' }, scope: { type: 'string', multiple: true },
  files: { type: 'string', multiple: true }, kind: { type: 'string', multiple: true },
  reuse: { type: 'boolean' }, all: { type: 'boolean' }, list: { type: 'boolean' }, 'no-build': { type: 'boolean' },
  'timeout-ms': { type: 'string' }, progress: { type: 'boolean' }, jobs: { type: 'string', default: '4' },
  'keep-failure': { type: 'boolean' }, 'test-name-pattern': { type: 'string' },
  'public-root': { type: 'string' }, 'consumer-root': { type: 'string' }, upstream: { type: 'string' },
} });
const jobs = Number(values.jobs);
const testTimeout = Number(values['timeout-ms'] ?? (values.all ? 7200000 : 300000));
if (!Number.isSafeInteger(testTimeout) || testTimeout < 1 || testTimeout > 7200000) throw Error('--timeout-ms must be 1..7200000.');
if (!Number.isInteger(jobs) || jobs < 1 || jobs > 4) throw Error('--jobs must be 1..4.');
for (const kind of values.kind ?? []) if (!['core', 'web', 'native', 'live'].includes(kind)) throw Error('Unknown test kind: ' + kind);
function git(args) {
  const r = spawnSync('git', args, { cwd: root, encoding: 'utf8', windowsHide: true });
  if (r.status !== 0) throw Error(r.stderr || 'Cannot determine changed files.');
  return r.stdout.split('\0').filter(Boolean);
}
// Explicit selections replace the default --changed from npm test.
if (values.upstream && !['hive', 'sdk'].includes(values.upstream)) throw Error('--upstream must be hive or sdk.');
const explicit = values.all || values.upstream || values.scope?.length || values.files?.length || positionals.length;
const changed = !explicit ? [...new Set([...git(['diff', '--name-only', '-z', values.since ?? 'HEAD']), ...git(['ls-files', '--others', '--exclude-standard', '-z'])])] : [];
const selected = selectTests(root, config, { changed, scopes: values.upstream ? config.scopes.filter(s => s !== 'tooling') : values.scope, files: [...(values.files ?? []), ...positionals], all: values.all, kinds: values.kind });
if (values.list) { for (const t of selected) console.log(`${t.scope}\t${t.kind}\t${t.file}`); }
else if (implicitSelectionTooLarge(selected.length, explicit)) {
  console.error(`Changed selection expands to ${selected.length} test files. Inspect with --list, select --files/--scope, or explicitly request --all. No build or tests started.`);
  process.exitCode = 1;
}
else if (selected.length === 0) {
  if (explicit || changed.some(f => /^(packages|services|ui|instructions|tools|specs|docs|tests)\//.test(f))) throw Error('No tests selected; specify the affected scope or files.');
} else {
  const parent = realpathSync(tmpdir()), temp = mkdtempSync(join(parent, 'ivy-tests-'));
  const env = { ...process.env, TMP: temp, TEMP: temp, TMPDIR: temp, IVY_TEST_TEMP: temp,
    IVY_TEST_PROGRESS: values.progress ? '1' : '0' };
  delete env.NODE_TEST_CONTEXT;
  let failed = true;
  async function run(args, cwd = root, stream = false, timeout = 180000) {
    const child = spawn(process.execPath, args, { cwd, env, windowsHide: true, detached: process.platform !== 'win32', stdio: ['ignore', 'pipe', 'pipe'] });
    let output = '', expired = false;
    const receive = bytes => { if (stream) process.stdout.write(bytes); else output = (output + bytes.toString()).slice(-12000); };
    child.stdout.on('data', receive); child.stderr.on('data', receive);
    let escalation;
    const stop = () => {
      if (!child.pid || child.exitCode !== null) return;
      if (process.platform === 'win32') {
        spawnSync('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore', timeout: 15000 });
      } else {
        try { process.kill(-child.pid, 'SIGTERM'); } catch { try { child.kill('SIGTERM'); } catch {} }
        escalation = setTimeout(() => {
          if (child.exitCode !== null) return;
          try { process.kill(-child.pid, 'SIGKILL'); } catch { try { child.kill('SIGKILL'); } catch {} }
        }, 2000);
      }
    };
    const interrupt = () => { expired = true; stop(); };
    process.once('SIGINT', interrupt); process.once('SIGTERM', interrupt);
    const timer = setTimeout(interrupt, timeout);
    const started = Date.now();
    const progress = values.progress ? setInterval(() => console.error(`Still running (${Math.round((Date.now() - started) / 1000)}s): node ${args.slice(0, 3).join(' ')}`), 30000) : null;
    try {
      const code = await new Promise((yes, no) => { child.on('error', no); child.on('close', yes); });
      if (code !== 0 || expired) throw Error(`${expired ? 'Timeout' : 'Failed'}: node ${args.slice(0, 3).join(' ')}\n${output}`);
    } finally { if (progress) clearInterval(progress); clearTimeout(timer); if (escalation) clearTimeout(escalation); process.removeListener('SIGINT', interrupt); process.removeListener('SIGTERM', interrupt); }
  }
  try {
    const inputCache = new Map();
    const inputs = selected.some(t => t.scope !== 'tooling') ? new Set(selected.flatMap(t => [...dependencies(root, t.file, inputCache)])) : new Set();
    const needsHive = !env.IVY_TEST_HIVE_URL && [...inputs].some(path =>
      /\.(mjs|ts)$/.test(path) && existsSync(join(root, path)) && /IVY_TEST_HIVE_(URL|CREDENTIAL)/.test(readFileSync(join(root, path), 'utf8')));
    const publicRoot = values['public-root'] ?? env.IVY_TEST_PUBLIC_ROOT ?? (config.privateRepo ? undefined : root);
    if (needsHive && (!publicRoot || !existsSync(resolve(publicRoot, 'dist/services/hive/src/server.js'))))
      throw Error('Set IVY_TEST_PUBLIC_ROOT (or --public-root) to the built public repo for isolated Hive tests.');
    const needsPython = config.privateRepo && [...inputs].some(path => /\.(mjs|ts)$/.test(path) && existsSync(join(root, path)) && /IVY_TEST_PYTHON/.test(readFileSync(join(root, path), 'utf8')));
    if (needsPython) {
      const python = spawnSync(env.IVY_TEST_PYTHON ?? 'python', ['--version'], { env, windowsHide: true, encoding: 'utf8', timeout: 5000 });
      if (python.error || python.status !== 0) throw Error('Selected tests need Python. Set IVY_TEST_PYTHON to an available interpreter before building or running tests.');
    }
    const testBuildStamp = resolve(root, 'dist/.test-build.json');
    const testBuildInputs = [resolve(root, 'tests'), resolve(root, 'dist/packages'), resolve(root, 'dist/services'), resolve(root, 'tsconfig.test.json'), resolve(root, 'tsconfig.runtime.json')];
    if (values['no-build'] && selected.some(t => t.scope !== 'tooling')) {
      let stamp;
      try { stamp = JSON.parse(readFileSync(testBuildStamp, 'utf8')); } catch {}
      if (!stamp?.fingerprint || stamp.fingerprint !== fingerprint(testBuildInputs, { selected: pathsForFingerprint(selected) }))
        throw Error('Test outputs are stale or have no source fingerprint; run without --no-build.');
    }
    if (!values['no-build'] && selected.some(t => t.scope !== 'tooling')) {
      if (selected.some(t => t.kind === 'web')) await run(['node_modules/vue-tsc/bin/vue-tsc.js', '-p', 'tsconfig.web.json', '--noEmit']);
        await run(['tools/build/build-backend.mjs', '--incremental', '--no-native']);
        // The web build replaces dist/apps with production bundles. Run it before the
        // test project emits imported ui modules that Node-based tests load directly.
        if (selected.some(t => t.kind === 'web')) await run(['tools/build/build-web.mjs',
          ...(selected.every(t => t.scope === 'data-collector') ? ['--ui', 'console,data-collector-ui'] : [])]);
        // The runtime build intentionally removes dist/tests; force the separate test
        // project to recreate its outputs instead of trusting stale incremental metadata.
        rmSync(resolve(root, 'dist/.test.tsbuildinfo'), { force: true });
        await run(['node_modules/typescript/bin/tsc', '-p', 'tsconfig.test.json', '--incremental', '--tsBuildInfoFile', 'dist/.test.tsbuildinfo']);
        if (selected.some(t => /phone-.*\.integration/.test(t.file)) && process.platform === 'win32') await run(['tools/testing/build-phone-tests.mjs'], root, false, 300000);
      mkdirSync(resolve(root, 'dist'), { recursive: true });
      writeFileSync(testBuildStamp, JSON.stringify({ fingerprint: fingerprint(testBuildInputs, { selected: pathsForFingerprint(selected) }) }));
    }
    const paths = selected.map(t => t.file.endsWith('.ts') ? 'dist/' + t.file.replace(/\.ts$/, '.js') : t.file);
    for (const path of paths) if (!existsSync(join(root, path))) throw Error('Missing test output: ' + path + '; build the selected tests first.');
    // Process-level Secretary tests share one isolated Hive and publish a singleton service.
    const concurrency = selected.some(t => /secretary-.*service\.test\.mjs$/.test(t.file)) ? 1 : jobs;
    const args = ['--test', '--test-concurrency=' + concurrency, '--test-reporter', pathToFileURL(resolve(root, 'tools/testing/test-reporter.mjs')).href,
      ...(values['test-name-pattern'] ? ['--test-name-pattern', values['test-name-pattern']] : []), ...paths];
    const reusable = values.reuse && !values.all && !needsHive && !values['test-name-pattern'] && !values['consumer-root'] && !env.IVY_TEST_PRIVATE_ROOT && selected.every(t => config.cacheable?.(t.file));
    const cacheInputs = reusable ? [...new Set(git(['ls-files', '-z', '--cached', '--others', '--exclude-standard']))].map(path => resolve(root, path)).concat(['packages', 'services', 'ui', 'instructions', 'tools', 'tests', 'specs', 'dist/packages', 'dist/services', 'dist/tests', 'dist/specs', 'node_modules/.package-lock.json'].map(path => resolve(root, path))) : [];
    const cacheKey = reusable ? fingerprint(cacheInputs, { files: paths, jobs, env: Object.fromEntries(Object.entries(process.env).sort()) }) : null;
    const cachePath = resolve(root, 'dist/.test-cache', 'passed.json');
    let previous;
    try { previous = JSON.parse(readFileSync(cachePath, 'utf8')); } catch {}
    const reuse = cacheKey && previous?.key === cacheKey;
    if (!reuse) rmSync(cachePath, { force: true });
    if (reuse) {
      if (values.progress) console.log('Reusing successful isolated tests; inputs unchanged.');
    } else if (needsHive) {
      await run([resolve(publicRoot, 'tools/testing/with-test-hive.mjs'), '--cwd', root, '--chat-fixture', '--timeout-ms', String(testTimeout), '--', ...args], root, true, testTimeout + 5000);
    } else await run(args, root, true, testTimeout);
    if (cacheKey && !reuse && cacheKey === fingerprint(cacheInputs, { files: paths, jobs, env: Object.fromEntries(Object.entries(process.env).sort()) })) {
      mkdirSync(dirname(cachePath), { recursive: true }); writeFileSync(cachePath, JSON.stringify({ key: cacheKey }));
    }
    const consumer = values['consumer-root'] ?? env.IVY_TEST_PRIVATE_ROOT;
    if (!config.privateRepo && consumer && (values.scope?.some(s => ['hive', 'sdk'].includes(s)) || changed.some(f => /^(?:services\/hive|packages\/(?:sdk|contracts))\//.test(f))))
      await run([resolve(consumer, 'tools/testing/test.mjs'), '--upstream', 'sdk', '--public-root', root,
        '--timeout-ms', String(testTimeout), ...(values.progress ? ['--progress'] : [])], resolve(consumer), true, testTimeout + 5000);
    failed = false;
  } catch (error) { console.error(error.message); process.exitCode = 1; }
  finally {
    if (failed && values['keep-failure']) console.error('Failure files (delete after diagnosis): ' + temp);
    else {
      const actual = realpathSync(temp), local = relative(parent, actual);
      if (isAbsolute(local) || !local.startsWith('ivy-tests-') || local.includes('/') || local.includes('\\')) throw Error('Unexpected test cleanup path.');
      rmSync(actual, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
    }
  }
}

function pathsForFingerprint(tests) { return tests.map(test => test.file).sort(); }
