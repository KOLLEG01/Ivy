import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { compileRustBackend } from '../tools/build/build-rust-backend.mjs';

function fixture(run, component = 'example') {
  const root = mkdtempSync(join(tmpdir(), 'ivy-rust-build-'));
  const manifest = join(root, `services/${component}/rust/Cargo.toml`);
  mkdirSync(join(dirname(manifest), 'src'), { recursive: true });
  writeFileSync(join(root, 'Cargo.toml'), `[workspace]\nresolver="3"\nmembers=["services/${component}/rust"]\n`);
  writeFileSync(join(root, 'Cargo.lock'), `version=4\n[[package]]\nname="ivy-${component}"\nversion="0.0.0"\n`);
  writeFileSync(join(root, 'rust-toolchain.toml'), readFileSync(new URL('../rust-toolchain.toml', import.meta.url)));
  writeFileSync(manifest, `[package]\nname="ivy-${component}"\nversion="0.0.0"\nedition="2024"\n[features]\nbrowser=[]\n[[bin]]\nname="ivy-${component}"\npath="src/main.rs"\nrequired-features=["browser"]\n[[bin]]\nname="ivy-${component}-helper"\npath="src/helper.rs"\n`);
  for (const name of ['main', 'helper']) writeFileSync(join(dirname(manifest), 'src', name + '.rs'), 'fn main(){println!("native-builder");}\n');
  const identity = { schemaVersion: 1, version: '1.2.3', buildId: 'sha256:' + 'a'.repeat(64) };
  const targets = [{ name: `ivy-${component}`, kind: ['bin'], 'required-features': ['browser'] }, { name: `ivy-${component}-helper`, kind: ['bin'] }, { name: 'ivy_example', kind: ['lib'] }];
  const calls = [];
  const cargo = (args, options) => {
    calls.push({ args, options });
    if (args[0] === 'metadata') return JSON.stringify({ workspace_members: ['selected'], packages: [{ id: 'selected', name: `ivy-${component}`, manifest_path: manifest, targets }] });
    return targets.filter(t => t.kind.includes('bin')).map(target => {
      const executable = join(options.env.CARGO_TARGET_DIR, 'x86_64-pc-windows-msvc/release', target.name + '.exe');
      mkdirSync(dirname(executable), { recursive: true }); writeFileSync(executable, 'fixture ' + target.name);
      return JSON.stringify({ reason: 'compiler-artifact', package_id: 'selected', target, executable });
    }).join('\n');
  };
  try { run({ root, manifest, identity, targets, calls, cargo, build: extra => compileRustBackend({ root, component, target: 'x86_64-pc-windows-msvc', identity, cargo, ...extra }) }); }
  finally { rmSync(root, { recursive: true, force: true }); }
}

test('native compilation selects real component binaries, required features and locked isolated outputs', () => fixture(f => {
  const result = f.build();
  assert.equal(result.files.length, 2);
  assert.equal(readFileSync(result.files[0], 'utf8'), 'fixture ivy-example');
  assert.deepEqual(JSON.parse(readFileSync(join(result.outputDirectory, 'build-info.json'))), f.identity);
  const { args, options } = f.calls[1];
  assert.ok(args.includes('--locked') && args.includes('--release'));
  assert.equal(args[args.indexOf('--features') + 1], 'browser');
  assert.equal(args[args.indexOf('--target') + 1], 'x86_64-pc-windows-msvc');
  assert.equal(options.env.IVY_BUILD_ID, f.identity.buildId);
  assert.equal(options.env.CARGO_TARGET_DIR, join(f.root, '.local/cargo/example'));
  assert.equal(existsSync(join(f.root, 'dist')), false);
  mkdirSync(join(f.root, '.cargo'));
  writeFileSync(join(f.root, '.cargo/config.toml'), '[build]\n');
  const configured = f.build();
  assert.notEqual(configured.rootInputFingerprint, result.rootInputFingerprint);
  assert.deepEqual(configured.identity, result.identity);
}));

test('native builds expose shared instruction assets outside target-specific executable directories', () => {
  for (const [component, source, exportName, name] of [
    ['secretary', 'instructions/secretary-notification-policy.ts', 'secretaryNotificationPolicy', 'secretary-notification-policy'],
    ['chat-bridge', 'services/chat-bridge/src/main-context.ts', 'mainInstructions', 'chat-main-instructions'],
  ]) fixture(f => {
    const path = join(f.root, source), policy = 'shared policy \ud800';
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, 'export const ' + exportName + ' = ' + JSON.stringify(policy) + ';\n');
    const result = f.build();
    assert.equal(result.files.length, 2);
    assert.deepEqual(result.instructionFiles, [join(f.root, `.local/build/rust/${component}/instructions/${name}.json`)]);
    assert.equal(JSON.parse(readFileSync(result.instructionFiles[0], 'utf8')), policy);
  }, component);
});

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

test('configured Cargo builds and executes the selected native fixture with required features', { skip: !process.env.IVY_CARGO }, () => fixture(f => {
  const target = new Map([
    ['win32-x64', 'x86_64-pc-windows-msvc'], ['win32-arm64', 'aarch64-pc-windows-msvc'],
    ['linux-x64', 'x86_64-unknown-linux-gnu'], ['linux-arm64', 'aarch64-unknown-linux-gnu'],
  ]).get(`${process.platform}-${process.arch}`);
  const result = f.build({ cargo: undefined, target });
  assert.equal(result.files.length, 2);
  for (const file of result.files) assert.equal(execFileSync(file, [], { encoding: 'utf8', windowsHide: true }).trim(), 'native-builder');
}));
