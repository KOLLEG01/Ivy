import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { compileRustBackend } from '../tools/build/build-rust-backend.mjs';
import { buildNativeDeployment } from '../tools/build/build-native-deployment.mjs';

function fixture(run, component = 'example') {
  const root = mkdtempSync(join(tmpdir(), 'ivy-rust-build-'));
  const manifest = join(root, `services/${component}/rust/Cargo.toml`);
  mkdirSync(join(dirname(manifest), 'src'), { recursive: true });
  writeFileSync(join(root, 'Cargo.toml'), '[workspace]\nresolver="3"\nmembers=["services/example/rust"]\n');
  writeFileSync(join(root, 'Cargo.lock'), 'version=4\n[[package]]\nname="ivy-example"\nversion="0.0.0"\n');
  writeFileSync(join(root, 'rust-toolchain.toml'), readFileSync(new URL('../rust-toolchain.toml', import.meta.url)));
  writeFileSync(manifest, '[package]\nname="ivy-example"\nversion="0.0.0"\nedition="2024"\n[features]\nbrowser=[]\n[[bin]]\nname="ivy-example"\npath="src/main.rs"\nrequired-features=["browser"]\n[[bin]]\nname="ivy-example-helper"\npath="src/helper.rs"\n');
  for (const name of ['main', 'helper']) writeFileSync(join(dirname(manifest), 'src', name + '.rs'), 'fn main(){println!("native-builder");}\n');
  const identity = { schemaVersion: 1, version: '1.2.3', buildId: 'sha256:' + 'a'.repeat(64) };
  const targets = [{ name: `ivy-${component}`, kind: ['bin'], 'required-features': ['browser'] }, { name: `ivy-${component}-helper`, kind: ['bin'] }, { name: 'ivy_example', kind: ['lib'] }];
  const calls = [];
  const cargo = (args, options) => {
    calls.push({ args, options });
    if (args[0] === 'metadata') return JSON.stringify({ workspace_members: ['selected'], packages: [{ id: 'selected', name: 'ivy-example', manifest_path: manifest, targets }] });
    return targets.filter(t => t.kind.includes('bin')).map(target => {
      const triple = args[args.indexOf('--target') + 1];
      const executable = join(options.env.CARGO_TARGET_DIR, triple, 'release', target.name + (triple.includes('windows') ? '.exe' : ''));
      mkdirSync(dirname(executable), { recursive: true }); writeFileSync(executable, 'fixture ' + target.name);
      return JSON.stringify({ reason: 'compiler-artifact', package_id: 'selected', target, executable });
    }).join('\n');
  };
  try { run({ root, manifest, identity, targets, calls, cargo, build: extra => compileRustBackend({ root, component, target: 'x86_64-pc-windows-msvc', identity, cargo, ...extra }) }); }
  finally { rmSync(root, { recursive: true, force: true }); }
}

for (const component of ['example', 'host-executor']) test(`native compilation selects real component binaries, required features and locked isolated outputs (${component})`, () => fixture(f => {
  const result = f.build();
  assert.equal(result.files.length, 2);
  assert.equal(readFileSync(result.files[0], 'utf8'), 'fixture ivy-' + component);
  assert.equal(f.calls[0].args[f.calls[0].args.indexOf('--manifest-path') + 1], f.manifest);
  assert.deepEqual(JSON.parse(readFileSync(join(result.outputDirectory, 'build-info.json'))), f.identity);
  const { args, options } = f.calls[1];
  assert.ok(args.includes('--locked') && args.includes('--release'));
  assert.equal(args[args.indexOf('--features') + 1], 'browser');
  assert.equal(args[args.indexOf('--target') + 1], 'x86_64-pc-windows-msvc');
  assert.equal(options.env.IVY_BUILD_ID, f.identity.buildId);
  assert.equal(options.env.CARGO_TARGET_DIR, join(f.root, '.local/cargo', component));
  assert.equal(existsSync(join(f.root, 'dist')), false);
}, component));

test('libraries and unregistered crates never fall back to TypeScript', () => fixture(f => {
  f.targets.splice(0, 2);
  assert.throws(() => f.build(), /no native service executable/);
  assert.equal(f.calls.length, 1);
  assert.throws(() => f.build({ cargo: () => JSON.stringify({ packages: [], workspace_members: [] }) }), /Register/);
}));

test('missing compiler artifacts cannot reuse old staged executable bytes', () => fixture(f => {
  const output = join(f.root, '.local/build/rust/example/win32-x64/ivy-example.exe');
  mkdirSync(dirname(output), { recursive: true }); writeFileSync(output, 'previous build');
  const cargo = (args, options) => args[0] === 'metadata' ? f.cargo(args, options) : '';
  assert.throws(() => f.build({ cargo }), /did not produce/);
  assert.equal(readFileSync(output, 'utf8'), 'previous build');
}));

test('foreign compiler artifacts and traversal names are rejected before staging', () => fixture(f => {
  const foreign = join(f.root, 'foreign.exe'); writeFileSync(foreign, 'foreign');
  const cargo = (args, options) => args[0] === 'metadata' ? f.cargo(args, options) : JSON.stringify({ reason: 'compiler-artifact', package_id: 'selected', target: f.targets[0], executable: foreign });
  assert.throws(() => f.build({ cargo }), /escaped/);
  const wrongTarget = join(f.root, '.local/cargo/example/aarch64-pc-windows-msvc/release/ivy-example.exe');
  mkdirSync(dirname(wrongTarget), { recursive: true }); writeFileSync(wrongTarget, 'wrong architecture');
  const wrongCargo = (args, options) => args[0] === 'metadata' ? f.cargo(args, options) : JSON.stringify({ reason: 'compiler-artifact', package_id: 'selected', target: f.targets[0], executable: wrongTarget });
  assert.throws(() => f.build({ cargo: wrongCargo }), /escaped/);
  assert.throws(() => f.build({ component: '../example' }), /component name/);
  assert.throws(() => f.build({ target: 'unsupported' }), /explicit supported Rust target/);
  assert.equal(existsSync(join(f.root, '.local/build')), false);
}));

for (const [os, target] of [['win32', 'x86_64-pc-windows-msvc'], ['linux', 'x86_64-unknown-linux-gnu']]) {
  test(`native Host deployment includes its automatic worker and consumed platform assets (${os})`, () => fixture(f => {
    const helpers = ['ivy-health', 'ivy-job', 'ivy-host-job', 'ivy-bootstrap-auto-worker'];
    const manifest = join(f.root, 'packages/host-runtime/rust/Cargo.toml');
    mkdirSync(dirname(manifest), { recursive: true });
    writeFileSync(manifest, '[package]\nname="ivy-host-runtime"\nversion="0.0.0"\n');
    const assets = ['install-windows.ps1', 'bootstrap-maintenance-windows.ps1', 'windows-principal.ps1'];
    for (const name of [...assets, 'unused.ps1']) {
      const path = join(f.root, 'packages/host-runtime/assets', name);
      mkdirSync(dirname(path), { recursive: true }); writeFileSync(path, 'fixture ' + name);
    }
    const plan = { schemaVersion: 1, componentId: 'host-executor', kind: 'service', version: f.identity.version,
      requirements: { node: '>=24 <25', os: ['win32', 'linux'], arch: ['x64'] },
      entrypoint: { executable: 'dist/native/ivy-host-executor', args: [], timeoutMs: 5000 },
      readiness: { timeoutMs: 5000, command: { executable: 'dist/native/ivy-health', args: [], timeoutMs: 1000 } } };
    writeFileSync(join(f.root, 'services/host-executor/deploy.json'), JSON.stringify(plan));
    const helperCalls = [];
    const cargo = (args, options) => {
      if (args[0] === 'metadata') {
        if (args[args.indexOf('--manifest-path') + 1] !== manifest) return f.cargo(args, options);
        return JSON.stringify({ workspace_members: ['helpers'], packages: [{ id: 'helpers', name: 'ivy-host-runtime',
          manifest_path: manifest, targets: helpers.map(name => ({ name, kind: ['bin'] })) }] });
      }
      if (args[args.indexOf('--package') + 1] !== 'ivy-host-runtime') return f.cargo(args, options);
      const names = args.filter((_, index) => args[index - 1] === '--bin');
      helperCalls.push(names);
      return names.map(name => {
        const executable = join(options.env.CARGO_TARGET_DIR, target, 'release', name + (os === 'win32' ? '.exe' : ''));
        mkdirSync(dirname(executable), { recursive: true }); writeFileSync(executable, 'fixture ' + name);
        return JSON.stringify({ reason: 'compiler-artifact', package_id: 'helpers', target: { name, kind: ['bin'] }, executable });
      }).join('\n');
    };
    const result = buildNativeDeployment({ component: 'host-executor', target, root: f.root, identity: f.identity, cargo });
    assert.deepEqual(helperCalls, [os === 'win32' ? helpers : ['ivy-health', 'ivy-bootstrap-auto-worker']]);
    const suffix = os === 'win32' ? '.exe' : '';
    const output = join(f.root, 'dist');
    assert.deepEqual(result.platforms, [os + '-x64']);
    assert.equal(result.plan.entrypoint.executable, `dist/native/${os}-x64/ivy-host-executor${suffix}`);
    assert.equal(readFileSync(join(output, `native/${os}-x64/ivy-bootstrap-auto-worker${suffix}`), 'utf8'), 'fixture ivy-bootstrap-auto-worker');
    for (const name of assets) {
      const path = join(output, 'packages/host-runtime/assets', name);
      assert.equal(existsSync(path), os === 'win32');
      if (os === 'win32') assert.equal(readFileSync(path, 'utf8'), 'fixture ' + name);
    }
    assert.equal(existsSync(join(output, 'packages/host-runtime/assets/unused.ps1')), false);
    assert.equal(existsSync(join(output, 'main.js')), false);
    assert.equal(existsSync(join(output, 'packages/host-runtime/src/bootstrap-auto-worker.js')), false);
  }, 'host-executor'));
}

test('configured Cargo builds and executes the selected native fixture with required features', { skip: !process.env.IVY_CARGO }, () => fixture(f => {
  const target = new Map([
    ['win32-x64', 'x86_64-pc-windows-msvc'], ['win32-arm64', 'aarch64-pc-windows-msvc'],
    ['linux-x64', 'x86_64-unknown-linux-gnu'], ['linux-arm64', 'aarch64-unknown-linux-gnu'],
  ]).get(`${process.platform}-${process.arch}`);
  const result = f.build({ cargo: undefined, target });
  assert.equal(result.files.length, 2);
  for (const file of result.files) assert.equal(execFileSync(file, [], { encoding: 'utf8', windowsHide: true }).trim(), 'native-builder');
}));
