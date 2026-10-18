import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, renameSync, rmSync, statSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, isAbsolute, join, relative } from 'node:path';
import { stageNativePayloads } from '../tools/build/stage-native-payloads.mjs';
import { nativeDispatcherSources } from '../tools/build/build-native-dispatchers.mjs';

const targets = [
  ['x86_64-pc-windows-msvc', 'win32-x64'], ['aarch64-pc-windows-msvc', 'win32-arm64'],
  ['x86_64-unknown-linux-gnu', 'linux-x64'], ['aarch64-unknown-linux-gnu', 'linux-arm64'],
];

function fixture(t, matrix = targets) {
  const base = realpathSync(tmpdir()), root = mkdtempSync(join(base, 'ivy-native-payload-'));
  t.after(() => {
    const child = relative(base, root);
    assert.ok(!isAbsolute(child) && !child.includes('/') && !child.includes('\\') && child.startsWith('ivy-native-payload-'));
    rmSync(root, { recursive: true, force: true });
  });
  const plan = {
    schemaVersion: 1, componentId: 'example', version: '1.2.3',
    requirements: { node: '>=24.18.0 <25.0.0' },
    entrypoint: { executable: 'node', args: ['dist/services/example/src/main.js'] },
    readiness: { command: { executable: 'node', args: ['dist/packages/host-runtime/src/health.js'] } },
    shutdown: { timeoutMs: 100 },
  };
  const planPath = join(root, 'services/example/deploy.json');
  mkdirSync(dirname(planPath), { recursive: true });
  const save = () => writeFileSync(planPath, JSON.stringify(plan)); save();
  const identity = { schemaVersion: 1, version: '9.8.7', buildId: 'sha256:' + 'a'.repeat(64) };
  // These bytes qualify assembly only. They are never executed or presented as
  // binaries built for the selected target.
  const bytes = name => Buffer.concat([Buffer.from([0, 255, 10]), Buffer.from(name)]);
  const payloads = matrix.flatMap(([target, platform]) => ['example', 'host-executor'].map(component => {
    const outputDirectory = join(root, 'inputs', component, platform);
    mkdirSync(outputDirectory, { recursive: true });
    const names = component === 'example' ? ['ivy-example'] : ['ivy-health', ...(platform.startsWith('win32-') ? ['ivy-job'] : [])];
    const files = names.map(name => {
      const file = join(outputDirectory, name + (platform.startsWith('win32-') ? '.exe' : ''));
      writeFileSync(file, bytes(basename(file))); return file;
    });
    writeFileSync(join(outputDirectory, 'build-info.json'), JSON.stringify(identity));
    const instructionFiles = [];
    if (component === 'example') {
      const file = join(root, 'inputs', 'instructions', platform, 'example-policy.json');
      mkdirSync(dirname(file), { recursive: true }); writeFileSync(file, JSON.stringify('policy \ud800'));
      instructionFiles.push(file);
    }
    return { component, target, platform, identity: { ...identity }, outputDirectory, files, instructionFiles };
  }));
  const outputRoot = join(root, 'dist'), requiredBinaries = { win32: ['ivy-job'] };
  const stage = extra => stageNativePayloads({ component: 'example', root, outputRoot, identity, payloads, requiredBinaries, ...extra });
  return { root, plan, planPath, save, identity, payloads, outputRoot, stage, bytes };
}

test('universal staging preserves every advertised payload, canonical shims and one package identity', t => {
  const f = fixture(t), originalPlan = readFileSync(f.planPath), rendered = nativeDispatcherSources({ root: f.root, component: 'example' });
  assert.equal(existsSync(f.outputRoot), false);
  const result = f.stage();
  assert.deepEqual(result.platforms, targets.map(([, platform]) => platform));
  const stamp = { schemaVersion: 1, componentId: 'example', version: f.plan.version, buildId: f.identity.buildId };
  assert.notEqual(stamp.version, f.identity.version);
  assert.deepEqual(result.stamp, stamp); assert.deepEqual(result.identity, f.identity);
  const stampBytes = readFileSync(join(f.outputRoot, 'build-info.json'));
  assert.deepEqual(JSON.parse(stampBytes), stamp);
  for (const payload of f.payloads) {
    const directory = join(f.outputRoot, 'native', payload.platform);
    assert.deepEqual(readFileSync(join(directory, 'build-info.json')), stampBytes);
    for (const input of payload.files) assert.deepEqual(readFileSync(join(directory, basename(input))), f.bytes(basename(input)));
  }
  for (const { path, source } of rendered.files) assert.equal(readFileSync(join(f.outputRoot, path), 'utf8'), source);
  assert.deepEqual(readFileSync(join(f.outputRoot, 'native/dispatch.mjs')), readFileSync(new URL('../tools/build/native-dispatch.mjs', import.meta.url)));
  assert.equal(JSON.parse(readFileSync(join(f.outputRoot, 'instructions/example-policy.json'))), 'policy \ud800');
  assert.deepEqual(readFileSync(f.planPath), originalPlan);
  assert.deepEqual(result.linuxExecutables.sort(), f.payloads.filter(p => p.platform.startsWith('linux-')).flatMap(p => p.files.map(file => join(f.outputRoot, 'native', p.platform, basename(file)))).sort());
  if (process.platform !== 'win32') for (const file of result.linuxExecutables) assert.equal(statSync(file).mode & 0o777, 0o755);
  assert.equal(new Set(result.files).size, result.files.length);
  assert.ok(result.files.every(file => existsSync(file)));
});

test('restricted unchanged plans stage exactly their advertised OS and architecture', t => {
  const f = fixture(t, [targets[2]]);
  f.plan.requirements.os = ['linux']; f.plan.requirements.arch = ['x64']; f.save();
  const result = f.stage();
  assert.deepEqual(result.platforms, ['linux-x64']);
  assert.equal(existsSync(join(f.outputRoot, 'native/win32-x64')), false);
});

test('missing, duplicate, extra and mismatched target records fail before writing output', t => {
  const cases = [
    [f => f.payloads.splice(0, 1), /selected component payload/],
    [f => f.payloads.push(f.payloads[0]), /Duplicate/],
    [f => { f.payloads[0].platform = 'linux-x64'; }, /advertised target/],
    [f => { f.plan.requirements.os = ['linux']; f.save(); }, /advertised target/],
    ...['constructor', 'toString', '__proto__'].map(target => [f => { f.payloads[0].target = target; }, /advertised target/]),
    [f => { f.plan.requirements.arch = []; f.save(); }, /platform matrix/],
    [f => { f.plan.requirements.os = null; f.save(); }, /platform matrix/],
    [f => { f.plan.requirements.os = ['win32', 'win32']; f.save(); }, /platform matrix/],
  ];
  for (const [mutate, error] of cases) {
    const f = fixture(t); mutate(f); assert.throws(() => f.stage(), error); assert.equal(existsSync(f.outputRoot), false);
  }
});

test('all targets require service, health and declared platform helpers with exact names', t => {
  for (const missing of ['ivy-example.exe', 'ivy-health.exe', 'ivy-job.exe']) {
    const f = fixture(t), payload = f.payloads.find(p => p.files.some(file => basename(file) === missing));
    // Leave the actual file in place: unselected stale bytes cannot satisfy a record.
    payload.files = payload.files.filter(file => basename(file) !== missing);
    assert.throws(() => f.stage(), /no executable files|executable is missing/);
    assert.equal(existsSync(f.outputRoot), false);
  }
  const f = fixture(t), payload = f.payloads[0], file = payload.files[0];
  payload.files[0] = join(dirname(file), 'Ivy-example.exe'); renameSync(file, payload.files[0]);
  assert.throws(() => f.stage(), /service executable is missing/);
  assert.equal(existsSync(f.outputRoot), false);
});

test('staging rejects changed identities, empty bytes, collisions and inconsistent shared instructions', t => {
  const cases = [
    [f => { f.payloads[0].identity.buildId = 'sha256:' + 'b'.repeat(64); }, /build identity differs/],
    [f => writeFileSync(join(f.payloads[1].outputDirectory, 'build-info.json'), JSON.stringify({ ...f.identity, version: 'stale' })), /build stamp differs/],
    [f => writeFileSync(f.payloads[0].files[0], ''), /selected native executable/],
    [f => f.payloads[1].files.push(f.payloads[0].files[0]), /contained regular file/],
    [f => {
      const payload = f.payloads[1], file = join(payload.outputDirectory, 'IVY-EXAMPLE.exe');
      writeFileSync(file, 'collision'); payload.files.push(file);
    }, /name collision/],
    [f => writeFileSync(f.payloads[2].instructionFiles[0], JSON.stringify('different policy')), /different shared instruction/],
  ];
  for (const [mutate, error] of cases) {
    const f = fixture(t); mutate(f); assert.throws(() => f.stage(), error); assert.equal(existsSync(f.outputRoot), false);
  }
});

test('staging rejects input escapes and directory links and preserves populated output', t => {
  const f = fixture(t), payload = f.payloads[0], original = payload.files[0];
  const outside = join(f.root, 'outside.exe'); writeFileSync(outside, 'outside payload'); payload.files[0] = outside;
  assert.throws(() => f.stage(), /contained regular file/); assert.equal(existsSync(f.outputRoot), false);
  payload.files[0] = original;
  const link = join(f.root, 'linked-input'); symlinkSync(payload.outputDirectory, link, process.platform === 'win32' ? 'junction' : 'dir');
  payload.outputDirectory = link;
  assert.throws(() => f.stage(), /directory must be contained without links/); assert.equal(existsSync(f.outputRoot), false);
  payload.outputDirectory = dirname(original);
  const directory = join(f.root, 'output-parent'); mkdirSync(directory);
  const linkedOutput = join(f.root, 'linked-output'); symlinkSync(directory, linkedOutput, process.platform === 'win32' ? 'junction' : 'dir');
  assert.throws(() => f.stage({ outputRoot: join(linkedOutput, 'dist') }), /output must not use links/);
  assert.equal(existsSync(join(directory, 'dist')), false);
  mkdirSync(f.outputRoot); const sentinel = join(f.outputRoot, 'keep.txt'); writeFileSync(sentinel, 'existing output');
  assert.throws(() => f.stage(), /fresh empty output/); assert.equal(readFileSync(sentinel, 'utf8'), 'existing output');
});

test('malformed legacy paths or Node requirements cannot leave partially staged payloads', t => {
  for (const change of [
    f => { f.plan.entrypoint.args[0] = 'dist/../escape.js'; },
    f => { f.plan.readiness.command.args[0] = f.plan.entrypoint.args[0]; },
    f => { f.plan.requirements.node = null; },
  ]) {
    const f = fixture(t); change(f); f.save();
    assert.throws(() => f.stage(), /relative Node entrypoint|must be distinct|unchanged v1 plan/);
    assert.equal(existsSync(f.outputRoot), false);
  }
});
