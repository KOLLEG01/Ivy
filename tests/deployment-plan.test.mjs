import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync, mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { materializeNativePlan } from '../packages/host-runtime/src/native-plan.mjs';
import { stageNativePayloads } from '../tools/build/stage-native-payloads.mjs';
import { buildNativeDeployment } from '../tools/build/build-native-deployment.mjs';
import { compileRustBackend } from '../tools/build/build-rust-backend.mjs';

const deployPlans = () => ['services', 'ui'].flatMap(owner => readdirSync(owner, { withFileTypes: true })
  .filter(entry => entry.isDirectory()).map(entry => join(owner, entry.name, 'deploy.json')).filter(existsSync));

test('deployment pipelines exclude tests and target only the requested component build', () => {
  const plans = deployPlans().map(path => JSON.parse(readFileSync(path, 'utf8')));
  for (const plan of plans) {
    assert.deepEqual(plan.checks, [], 'Deployment does not run general schema/UI checks; targeted tests run separately.');
    if (plan.kind !== 'app') assert.ok(plan.readiness.command, 'Activation still verifies readiness');
    for (const command of plan.prepare) {
      assert.ok(!command.args.some(arg => /(^|\/)(tests?([/.:-]|$)|build-phone-tests|check-phone-runtime)/.test(arg)), plan.componentId);
    }
  }
  for (const plan of plans) {
    const build = plan.prepare.find(command => command.args.some(arg =>
      ['tools/build/build-deployment.mjs', 'tools/build/build-native-deployment.mjs'].includes(arg)));
    assert.ok(build, plan.componentId + ' must have a deployment build');
    const componentIndex = build.args.indexOf('--component');
    assert.equal(build.args[componentIndex + 1], plan.componentId, plan.componentId + ' must build itself');
    assert.deepEqual(plan.checks, [], plan.componentId + ' must not run a second checked pipeline');
  }
});

for (const component of ['host-executor', 'service-manager', 'hive', 'data-collector', 'dashboards', 'task-board', 'agent-manager']) test(`native source support and selected host commands (${component})`, () => {
  const source = JSON.parse(readFileSync(`services/${component}/deploy.json`, 'utf8'));
  for (const os of ['win32', 'linux']) for (const arch of ['x64', 'arm64']) {
    const selected = materializeNativePlan(source, { os, arch });
    const extension = os === 'win32' ? '.exe' : '';
    assert.equal(selected.entrypoint.executable, `dist/native/${os}-${arch}/ivy-${component}${extension}`);
    assert.equal(selected.readiness.command.executable, `dist/native/${os}-${arch}/ivy-health${extension}`);
    assert.deepEqual(selected.requirements.os, [os]);
    assert.deepEqual(selected.requirements.arch, [arch]);
    assert.deepEqual(selected.prepare, source.prepare);
    assert.deepEqual(materializeNativePlan(selected, { os, arch }), selected);
    assert.throws(() => materializeNativePlan(selected, { os: os === 'win32' ? 'linux' : 'win32', arch }));
  }
  assert.deepEqual(source.requirements.os, ['win32', 'linux']);
  assert.equal(source.entrypoint.executable, `dist/native/ivy-${component}`);
  const javascript = { ...source, entrypoint: { executable: 'node', args: ['worker.mjs'] } };
  assert.deepEqual(materializeNativePlan(javascript), javascript);
  assert.throws(() => materializeNativePlan(source, { os: 'darwin', arch: 'x64' }));
  assert.throws(() => materializeNativePlan({ ...source, readiness: { command: { executable: 'node' } } }));
});

test('native staging needs only the selected host and emits no Node entry shim', t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-native-plan-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const component = 'fixture', target = 'x86_64-pc-windows-msvc', platform = 'win32-x64';
  const source = JSON.parse(readFileSync('services/data-collector/deploy.json', 'utf8'));
  source.componentId = component; source.entrypoint.executable = 'dist/native/ivy-fixture';
  mkdirSync(join(root, 'services', component), { recursive: true });
  writeFileSync(join(root, 'services', component, 'deploy.json'), JSON.stringify(source));
  const identity = { schemaVersion: 1, version: source.version, buildId: 'sha256:' + '1'.repeat(64) };
  const outputDirectory = join(root, 'payload'); mkdirSync(outputDirectory);
  writeFileSync(join(outputDirectory, 'build-info.json'), JSON.stringify(identity));
  const files = ['ivy-fixture.exe', 'ivy-health.exe'].map(name => {
    const path = join(outputDirectory, name); writeFileSync(path, 'synthetic assembly input'); return path;
  });
  const payload = { component, target, platform, identity, outputDirectory, files, instructionFiles: [] };
  const plan = materializeNativePlan(source, { os: 'win32', arch: 'x64' });
  const outputRoot = join(root, 'dist');
  const result = stageNativePayloads({ component, root: resolve(root), outputRoot, identity, payloads: [payload], plan });
  assert.deepEqual(result.platforms, [platform]);
  assert.ok(existsSync(join(root, plan.entrypoint.executable)));
  assert.ok(existsSync(join(root, plan.readiness.command.executable)));
  assert.ok(!existsSync(join(outputRoot, 'services')));
  assert.ok(!existsSync(join(outputRoot, 'packages')));
  assert.ok(!existsSync(join(outputRoot, 'native/dispatch.mjs')));
  assert.ok(!existsSync(join(outputRoot, 'native/linux-x64')));
  assert.throws(() => stageNativePayloads({ component, root, outputRoot: join(root, 'missing'), identity, plan,
    payloads: [{ ...payload, files: [files[0]] }] }), /Required native executable is missing/);
  assert.ok(!existsSync(join(root, 'missing')));
});

for (const component of ['fixture', 'host-executor', 'task-board']) test(`normal native build selects one target and registered helpers (${component})`, t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-native-build-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const previousTarget = process.env.CARGO_TARGET_DIR, previousCache = process.env.IVY_BUILD_CACHE_ROOT;
  t.after(() => {
    for (const [name, value] of [['CARGO_TARGET_DIR', previousTarget], ['IVY_BUILD_CACHE_ROOT', previousCache]]) {
      if (value === undefined) delete process.env[name]; else process.env[name] = value;
    }
  });
  let targetDirectory = join(root, 'explicit-target');
  process.env.CARGO_TARGET_DIR = targetDirectory;
  process.env.IVY_BUILD_CACHE_ROOT = join(root, 'prepared-cache');
  const source = JSON.parse(readFileSync('services/data-collector/deploy.json', 'utf8'));
  source.componentId = component; source.entrypoint.executable = `dist/native/ivy-${component}`;
  const serviceManifest = join(root, `services/${component}/rust/Cargo.toml`);
  const helperManifest = join(root, 'packages/host-runtime/rust/Cargo.toml');
  for (const path of [`services/${component}/rust`, 'packages/host-runtime/rust']) mkdirSync(join(root, path), { recursive: true });
  writeFileSync(serviceManifest, '# synthetic Cargo metadata fixture\n');
  writeFileSync(helperManifest, '# synthetic Cargo metadata fixture\n');
  writeFileSync(join(root, `services/${component}/deploy.json`), JSON.stringify(source));
  if (component === 'host-executor') for (const name of ['install-windows.ps1', 'bootstrap-maintenance-windows.ps1', 'windows-principal.ps1']) {
    const path = join(root, 'packages/host-runtime/assets', name);
    mkdirSync(dirname(path), { recursive: true }); writeFileSync(path, 'synthetic consumed asset');
  }
  const identity = { schemaVersion: 1, version: source.version, buildId: 'sha256:' + '2'.repeat(64) };
  const packages = [
    { id: 'fixture', name: `ivy-${component}`, manifest_path: serviceManifest, targets: [
      { name: `ivy-${component}`, kind: ['bin'] },
      ...(component === 'host-executor' ? [{ name: 'ivy-bootstrap-auto-worker', kind: ['bin'] }] : []),
    ] },
    { id: 'helpers', name: 'ivy-host-runtime', manifest_path: helperManifest, targets: ['ivy-health', 'ivy-job', 'ivy-host-job'].map(name => ({ name, kind: ['bin'] })) },
  ];
  const calls = [];
  const cargo = (args, options) => {
    calls.push(args);
    assert.equal(options.env.CARGO_TARGET_DIR, targetDirectory);
    if (args[0] === 'metadata') return JSON.stringify({ packages, workspace_members: packages.map(pkg => pkg.id) });
    const pkg = packages.find(pkg => pkg.name === args[args.indexOf('--package') + 1]);
    const target = args[args.indexOf('--target') + 1];
    const directory = join(options.env.CARGO_TARGET_DIR, target, 'release'); mkdirSync(directory, { recursive: true });
    assert.equal(options.env.IVY_BUILD_ID, identity.buildId);
    const binaries = args.flatMap((arg, index) => arg === '--bin' ? [args[index + 1]] : []);
    return binaries.map(name => {
      const executable = join(directory, name + '.exe'); writeFileSync(executable, 'synthetic compiler artifact');
      return JSON.stringify({ reason: 'compiler-artifact', package_id: pkg.id, target: { name, kind: ['bin'] }, executable });
    }).join('\n');
  };
  const runtimeNames = ['icudt78.dll', 'icuuc78.dll', 'icuin78.dll', 'LICENSE.icu'];
  if (component === 'task-board') {
    const catalogs = join(root, 'specs/native/fixture'); mkdirSync(catalogs, { recursive: true });
    writeFileSync(join(catalogs, 'catalog.json'), '{}');
    const runtime = join(root, '.local/native-runtime/win32-x64'); mkdirSync(runtime, { recursive: true });
    for (const name of runtimeNames.slice(0, -1)) writeFileSync(join(runtime, name), 'selected runtime ' + name);
    assert.throws(() => buildNativeDeployment({ component, root, target: 'x86_64-pc-windows-msvc', identity, cargo }));
    assert.equal(calls.length, 0, 'Incomplete runtime inputs must fail before Cargo or staging');
    writeFileSync(join(runtime, 'LICENSE.icu'), 'selected runtime LICENSE.icu');
  }
  const built = buildNativeDeployment({ component, root, target: 'x86_64-pc-windows-msvc', identity, cargo });
  assert.deepEqual(built.platforms, ['win32-x64']);
  assert.equal(built.staged, true);
  const builds = calls.filter(args => args[0] === 'build');
  assert.equal(builds.length, 2);
  assert.ok(builds.every(args => args[args.indexOf('--target') + 1] === 'x86_64-pc-windows-msvc'));
  assert.ok(existsSync(join(root, built.plan.entrypoint.executable)));
  assert.ok(existsSync(join(root, built.plan.readiness.command.executable)));
  assert.equal(existsSync(join(root, 'dist/native/win32-x64/ivy-host-job.exe')), component === 'host-executor');
  if (component === 'task-board') {
    for (const name of runtimeNames) assert.equal(readFileSync(join(root, 'dist/native/win32-x64', name), 'utf8'), 'selected runtime ' + name);
    assert.equal(existsSync(join(root, 'dist/specs/native/fixture/catalog.json')), true);
    assert.equal(existsSync(join(root, 'dist/services/task-board/src/main.js')), false);
  }
  delete process.env.CARGO_TARGET_DIR;
  targetDirectory = join(root, 'prepared-cache', 'cargo');
  compileRustBackend({ component, root, target: 'x86_64-pc-windows-msvc', identity, cargo });
  delete process.env.IVY_BUILD_CACHE_ROOT;
  targetDirectory = join(root, '.local', 'cargo', component);
  compileRustBackend({ component, root, target: 'x86_64-pc-windows-msvc', identity, cargo });
});
