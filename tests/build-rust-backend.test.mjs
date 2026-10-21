import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { execFileSync } from 'node:child_process';
import { compileRustBackend, compileRustHelpers } from '../tools/build/build-rust-backend.mjs';

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
  const options = { root, component, target: 'x86_64-pc-windows-msvc', identity, cargo };
  try { run({ root, manifest, identity, targets, calls, cargo,
    build: extra => compileRustBackend({ ...options, ...extra }),
    helpers: extra => compileRustHelpers({ ...options, binaries: [`ivy-${component}-helper`], ...extra }) }); }
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
    if (component === 'chat-bridge') writeFileSync(join(dirname(path), 'notice-input.ts'), 'export const noticePrompt=(callerPrincipalId,text)=>`before ${text} between ${noticePrefix(callerPrincipalId)} after`;');
    const result = f.build();
    assert.equal(result.files.length, 2);
    assert.deepEqual(result.instructionFiles, [join(f.root, `.local/build/rust/${component}/instructions/${name}.json`), ...(component === 'chat-bridge' ? [join(f.root, '.local/build/rust/chat-bridge/instructions/chat-notice-prompt.json')] : [])]);
    assert.equal(JSON.parse(readFileSync(result.instructionFiles[0], 'utf8')), policy);
  }, component);
});

test('explicit target runtime files accompany binaries without entering the executable set', () => fixture(f => {
  const directory = join(f.root, 'runtime'); mkdirSync(directory);
  const runtimeFiles = ['example_runtime.dll', 'LICENSE.example'].map(name => {
    const file = join(directory,name); writeFileSync(file,'runtime '+name); return file;
  });
  const result=f.build({runtimeFiles});
  assert.equal(result.files.length,2);
  assert.equal(result.runtimeFiles.length,2);
  for(const [i,file] of result.runtimeFiles.entries()) assert.deepEqual(readFileSync(file),readFileSync(runtimeFiles[i]));
  const before=f.calls.length;
  assert.throws(()=>f.build({runtimeFiles:[join(directory,'missing.dll')]}));
  assert.equal(f.calls.length,before);
  const wrong=join(directory,'libwrong.so.78');writeFileSync(wrong,'other OS');
  assert.throws(()=>f.build({runtimeFiles:[wrong]}),/runtime filename/);
  assert.equal(f.calls.length,before);
}));

test('libraries and unregistered crates never fall back to TypeScript', () => fixture(f => {
  f.targets.splice(0, 2);
  assert.throws(() => f.build(), /no native service executable/);
  assert.equal(f.calls.length, 1);
  assert.throws(() => f.build({ cargo: () => JSON.stringify({ packages: [], workspace_members: [] }) }), /Register/);
}));

test('helper-only compilation selects exact bins and their features without requiring a service', () => fixture(f => {
  f.targets.shift();
  f.targets[0]['required-features'] = ['worker'];
  f.targets.push({ name: 'other-helper', kind: ['bin'], 'required-features': ['unselected'] });
  const runtime = join(f.root, 'example_runtime.dll'); writeFileSync(runtime, 'runtime');
  const result = f.helpers({ runtimeFiles: [runtime] });
  assert.equal(result.files.length, 1);
  assert.equal(readFileSync(result.files[0], 'utf8'), 'fixture ivy-example-helper');
  assert.deepEqual(result.runtimeFiles, [join(result.outputDirectory, 'example_runtime.dll')]);
  assert.deepEqual(result.instructionFiles, []);
  assert.deepEqual(JSON.parse(readFileSync(join(result.outputDirectory, 'build-info.json'))), f.identity);
  const { args } = f.calls[1];
  assert.deepEqual(args.filter((_, i) => args[i - 1] === '--bin'), ['ivy-example-helper']);
  assert.equal(args[args.indexOf('--features') + 1], 'worker');
  assert.equal(args.includes('--no-default-features'), false);
  assert.ok(args.includes('--locked') && args.includes('--release'));
  assert.throws(() => f.build(), /no native service executable/);
}));

test('helper names must be distinct exact registered bins and exclude the service', () => fixture(f => {
  for (const binaries of [undefined, null, [], 'ivy-example-helper', ['../helper'], ['helper.exe'],
    ['ivy-example-helper', 'ivy-example-helper'], ['Helper', 'helper'], ['ivy-example'],
    ['missing'], ['ivy_example'], ['IVY-example-helper']]) {
    assert.throws(() => f.helpers({ binaries }), /helpers require|not a helper|exact native helper/);
  }
  assert.equal(f.calls.some(call => call.args[0] === 'build'), false);
  assert.equal(existsSync(join(f.root, '.local/build')), false);
}));

test('helper artifacts are complete and contained before any old output is replaced', () => fixture(f => {
  const output = join(f.root, '.local/build/rust/example/win32-x64/ivy-example-helper.exe');
  mkdirSync(dirname(output), { recursive: true }); writeFileSync(output, 'previous helper');
  const metadata = (args, options) => f.cargo(args, options);
  const absent = (args, options) => args[0] === 'metadata' ? metadata(args, options) : '';
  assert.throws(() => f.helpers({ cargo: absent }), /did not produce ivy-example-helper/);
  const foreign = join(f.root, 'foreign.exe'); writeFileSync(foreign, 'foreign');
  const escaped = (args, options) => args[0] === 'metadata' ? metadata(args, options) : JSON.stringify({
    reason: 'compiler-artifact', package_id: 'selected', target: f.targets[1], executable: foreign,
  });
  assert.throws(() => f.helpers({ cargo: escaped }), /escaped/);
  assert.equal(readFileSync(output, 'utf8'), 'previous helper');
  assert.equal(existsSync(join(dirname(output), 'build-info.json')), false);
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
  for (const target of ['constructor', 'toString', '__proto__']) assert.throws(() => f.build({ target }), /explicit supported Rust target/);
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
