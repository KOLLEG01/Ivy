import test from 'node:test';
import assert from 'node:assert/strict';
import { copyFileSync, linkSync, mkdirSync, mkdtempSync, renameSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { tmpdir } from 'node:os';
import { spawn, spawnSync } from 'node:child_process';
import { once } from 'node:events';
import { buildNativeDispatchers } from '../tools/build/build-native-dispatchers.mjs';
import { nativeExecutable } from '../tools/build/native-dispatch.mjs';

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'ivy-native-dispatch-'));
  t.after(() => {
    assert.ok(relative(tmpdir(), root).startsWith('ivy-native-dispatch-'));
    rmSync(root, { recursive: true, force: true });
  });
  const plan = { componentId: 'example', entrypoint: { executable: 'node', args: ['dist/services/example/src/main.js'] }, readiness: { command: { executable: 'node', args: ['dist/packages/host-runtime/src/health.js'] } }, shutdown: { timeoutMs: 100 } };
  mkdirSync(join(root, 'services/example'), { recursive: true });
  const save = () => writeFileSync(join(root, 'services/example/deploy.json'), JSON.stringify(plan));
  save();
  const result = buildNativeDispatchers({ root, component: 'example' });
  const native = join(root, 'dist/native'), executable = name => join(native, `${process.platform}-${process.arch}`, name + (process.platform === 'win32' ? '.exe' : ''));
  const install = (name, source = process.execPath) => { const path = executable(name); mkdirSync(dirname(path), { recursive: true }); try { linkSync(source, path); } catch { copyFileSync(source, path); } return path; };
  const run = (health, args = [], options = {}) => spawnSync(process.execPath, [result.files[health ? 2 : 1], ...args], { windowsHide: true, timeout: 10000, ...options });
  return { root, plan, save, result, native, executable, install, run };
}

test('native dispatchers forward actual process streams, arguments, environment and exit codes', t => {
  const f = fixture(t); f.install('ivy-example'); f.install('ivy-health');
  const script = join(f.root, 'peer.cjs');
  writeFileSync(script, `const fs=require('node:fs');const input=fs.readFileSync(0);process.stdout.write(JSON.stringify({args:process.argv.slice(2),node:process.env.IVY_NODE_EXECUTABLE,launch:process.env.IVY_LAUNCH_ID,pid:process.pid,parent:process.ppid})+'\\n');process.stdout.write(input);process.stderr.write(Buffer.from([0,255,10]));process.exitCode=17;`);
  const input = Buffer.from([0, 255, 13, 10, 1]);
  for (const moduleType of ['commonjs', 'module']) {
    writeFileSync(join(f.root, 'package.json'), JSON.stringify({ type: moduleType }));
    const r = f.run(false, [script, 'space argument', '"quoted"', 'text😀'], { input, env: { ...process.env, IVY_NODE_EXECUTABLE: 'obsolete', IVY_LAUNCH_ID: 'fixture-launch' } });
    assert.ifError(r.error); assert.equal(r.status, 17);
    const line = r.stdout.indexOf(10), value = JSON.parse(r.stdout.subarray(0, line));
    assert.deepEqual(value.args, ['space argument', '"quoted"', 'text😀']);
    assert.equal(value.node, process.execPath); assert.equal(value.launch, 'fixture-launch');
    assert.equal(value.parent, r.pid); assert.notEqual(value.pid, r.pid);
    assert.deepEqual(r.stdout.subarray(line + 1), input); assert.deepEqual(r.stderr, Buffer.from([0,255,10]));
    assert.equal(f.run(true, [script], { input }).status, 17);
  }
});

test('native dispatch fails before launch for missing, foreign or unsupported payloads', t => {
  const f = fixture(t);
  const missing = f.run(true, [], { encoding: 'utf8' });
  assert.equal(missing.status, 1); assert.deepEqual(JSON.parse(missing.stdout), { ok: false, code: 'artifact_missing' });
  assert.equal(missing.stderr, '');
  renameSync(f.result.files[0], f.result.files[0] + '.saved');
  const missingDispatcher = f.run(true, [], { encoding: 'utf8' });
  assert.equal(missingDispatcher.status, 1); assert.deepEqual(JSON.parse(missingDispatcher.stdout), { ok: false, code: 'artifact_missing' });
  assert.equal(missingDispatcher.stderr, '');
  renameSync(f.result.files[0] + '.saved', f.result.files[0]);
  for (const platform of ['win32', 'linux']) for (const arch of ['x64', 'arm64']) {
    const path = join(f.native, `${platform}-${arch}`, 'ivy-example' + (platform === 'win32' ? '.exe' : ''));
    mkdirSync(dirname(path), { recursive: true }); writeFileSync(path, 'selection fixture only');
    assert.equal(nativeExecutable(f.native, 'ivy-example', platform, arch), path);
  }
  assert.throws(() => nativeExecutable(f.native, 'ivy-example', 'darwin', 'x64'), { code: 'unsupported_platform' });
  assert.throws(() => nativeExecutable(f.native, '../foreign'), { code: 'invalid_arguments' });
  const foreign = join(f.root, 'foreign'); mkdirSync(foreign);
  writeFileSync(join(foreign, 'ivy-health' + (process.platform === 'win32' ? '.exe' : '')), 'foreign fixture');
  const linkedRoot = join(f.root, 'linked-native'); mkdirSync(linkedRoot);
  symlinkSync(foreign, join(linkedRoot, `${process.platform}-${process.arch}`), process.platform === 'win32' ? 'junction' : 'dir');
  assert.throws(() => nativeExecutable(linkedRoot, 'ivy-health'), { code: 'artifact_missing' });
  f.plan.entrypoint.args[0] = 'dist/../outside.js'; f.save();
  assert.throws(() => buildNativeDispatchers({ root: f.root, component: 'example' }), /relative Node entrypoint/);
});

test('native dispatch bounds signal forwarding without starting another process group', { skip: process.platform !== 'linux' }, async t => {
  const f = fixture(t); f.install('ivy-example');
  const script = join(f.root, 'signal.cjs');
  writeFileSync(script, "process.on('SIGTERM',()=>{});process.stdout.write('ready\\n');setInterval(()=>{},1000);");
  const child = spawn(process.execPath, [f.result.files[1], script], { stdio: ['ignore', 'pipe', 'pipe'] });
  const timer = setTimeout(() => child.kill('SIGKILL'), 5000);
  t.after(() => { clearTimeout(timer); child.kill('SIGKILL'); });
  await once(child.stdout, 'data');
  const exited = once(child, 'exit'); child.kill('SIGTERM');
  assert.deepEqual(await exited, [null, 'SIGKILL']);
});

test('native legacy paths execute a real Rust backend and health entrypoint', { skip: !process.env.IVY_CARGO }, t => {
  const f = fixture(t), source = join(f.root, 'rust'); mkdirSync(source);
  writeFileSync(join(source, 'Cargo.toml'), '[package]\nname="ivy-dispatch-fixture"\nversion="0.0.0"\nedition="2024"\n[[bin]]\nname="ivy-example"\npath="main.rs"\n');
  writeFileSync(join(source, 'Cargo.lock'), 'version=4\n[[package]]\nname="ivy-dispatch-fixture"\nversion="0.0.0"\n');
  writeFileSync(join(source, 'main.rs'), `use std::{env,io::{self,Read,Write}};fn main(){let args:Vec<_>=env::args().skip(1).collect();if args==["health"]{println!("{{\\\"ok\\\":true,\\\"buildId\\\":\\\"fixture\\\",\\\"generation\\\":null}}");return;}let mut input=Vec::new();io::stdin().read_to_end(&mut input).unwrap();println!("{}|{}|{}",args.join("|"),env::var("IVY_NODE_EXECUTABLE").unwrap(),env::var("IVY_LAUNCH_ID").unwrap());io::stdout().write_all(&input).unwrap();eprintln!("rust diagnostic");std::process::exit(23);}`);
  const built = spawnSync(process.env.IVY_CARGO, ['build', '--locked', '--offline', '--manifest-path', join(source, 'Cargo.toml'), '--target-dir', join(source, 'target')], { windowsHide: true, encoding: 'utf8', timeout: 120000 });
  assert.ifError(built.error); assert.equal(built.status, 0, built.stderr);
  const binary = join(source, 'target/debug/ivy-example' + (process.platform === 'win32' ? '.exe' : ''));
  f.install('ivy-example', binary); f.install('ivy-health', binary);
  const input = Buffer.from([0,255,10]);
  const r = f.run(false, ['space argument', 'text😀'], { input, env: { ...process.env, IVY_LAUNCH_ID: 'rust-launch' } });
  assert.equal(r.status, 23, r.stderr?.toString());
  assert.deepEqual(r.stdout, Buffer.concat([Buffer.from(`space argument|text😀|${process.execPath}|rust-launch\n`), input]));
  assert.equal(r.stderr.toString(), 'rust diagnostic\n');
  const health = f.run(true, ['health'], { encoding: 'utf8' });
  assert.equal(health.status, 0, health.stderr); assert.deepEqual(JSON.parse(health.stdout), { ok: true, buildId: 'fixture', generation: null });
});
