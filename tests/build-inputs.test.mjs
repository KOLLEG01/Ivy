import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';
import { buildBackend, backendComponents } from '../tools/build/build-backend.mjs';

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'ivy-build-inputs-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const write = (path, value) => { mkdirSync(dirname(join(root, path)), { recursive: true }); writeFileSync(join(root, path), value); };
  for (const component of ['hive', 'host-executor', 'phone-bridge']) write('services/' + component + '/deploy.json', JSON.stringify({
    componentId: component, entrypoint: { executable: 'dist/native/ivy-' + component },
  }));
  write('services/example/deploy.json', JSON.stringify({ entrypoint: { executable: 'node' } }));
  write('dist/services/retired/main.js', 'obsolete backend');
  write('dist/specs/retired.json', 'obsolete contract');
  const calls = [], identity = { schemaVersion: 1, version: '1.0.0', buildId: 'sha256:' + '1'.repeat(64) };
  const prepare = async ({ component, outputRoot, identity: actual, target }) => {
    assert.deepEqual(actual, identity);
    assert.equal(existsSync(join(outputRoot, 'retired.json')), false);
    calls.push({ component, target });
    mkdirSync(join(outputRoot, 'native'), { recursive: true });
    writeFileSync(join(outputRoot, 'native', 'ivy-' + component), component);
    writeFileSync(join(outputRoot, 'build-info.json'), JSON.stringify({ ...identity, componentId: component }));
    if (component === 'host-executor') for (const name of ['ivy', 'ivy-bootstrap-auto-worker', 'ivy-health'])
      writeFileSync(join(outputRoot, 'native', name), name);
  };
  return { root, calls, identity, native: prepare, phone: prepare };
}

test('default and aggregate backend preparation builds registered native components and removes retired emission', async t => {
  const f = fixture(t);
  assert.deepEqual(backendComponents(f.root), ['hive', 'host-executor', 'phone-bridge']);
  await buildBackend(f);
  assert.deepEqual(f.calls.map(call => call.component), ['hive', 'host-executor', 'phone-bridge']);
  for (const name of ['ivy-hive', 'ivy-host-executor', 'ivy-phone-bridge', 'ivy', 'ivy-bootstrap-auto-worker', 'ivy-health'])
    assert.ok(existsSync(join(f.root, 'dist/native', name)), name);
  assert.equal(existsSync(join(f.root, 'dist/services')), false);
  assert.equal(existsSync(join(f.root, 'dist/specs/retired.json')), false);
  assert.deepEqual(JSON.parse(readFileSync(join(f.root, 'dist/build-info.json'), 'utf8')), f.identity);
});

test('explicit component preparation keeps its own stamp, target and payload without compiling unrelated services', async t => {
  const f = fixture(t);
  await buildBackend({ ...f, component: 'hive', target: 'x86_64-unknown-linux-gnu' });
  assert.deepEqual(f.calls, [{ component: 'hive', target: 'x86_64-unknown-linux-gnu' }]);
  assert.equal(existsSync(join(f.root, 'dist/native/ivy-phone-bridge')), false);
  assert.equal(JSON.parse(readFileSync(join(f.root, 'dist/build-info.json'), 'utf8')).componentId, 'hive');
  await assert.rejects(buildBackend({ ...f, component: 'example' }), /Unknown native component/);
  await assert.rejects(buildBackend({ ...f, target: 'x86_64-unknown-linux-gnu' }), /explicit native component/);
});

test('native preparation failures are propagated without a legacy compiler fallback', async t => {
  const f = fixture(t);
  await assert.rejects(buildBackend({ ...f, component: 'hive', native: () => { throw Error('compiler failed'); } }), /compiler failed/);
  assert.deepEqual(f.calls, []);
  assert.equal(existsSync(join(f.root, 'dist/services')), false);
});

test('normal, aggregate and development commands select the native coordinator', t => {
  const f = fixture(t), record = join(f.root, 'calls.jsonl'), hook = join(f.root, 'route-hook.mjs');
  const log = String.raw`import {appendFileSync} from 'node:fs';const record=value=>appendFileSync(${JSON.stringify(record)},JSON.stringify(value)+'\n');`;
  const identity = JSON.stringify(f.identity);
  const modules = Object.fromEntries([
    ['build-backend.mjs', `${log}export const backendComponents=()=>['chat-bridge'];export async function buildBackend(options={}){record(['backend',options.component??null]);return ${identity};}`],
    ['build-sdk.mjs', `${log}export function buildSdk(){record(['sdk']);}`],
    ['build-web.mjs', `${log}export async function buildWeb(){record(['web']);}`],
  ].map(([name, source]) => [pathToFileURL(resolve('tools/build', name)).href, source]));
  writeFileSync(hook, `import {registerHooks} from 'node:module';
    const modules=${JSON.stringify(modules)};
    registerHooks({resolve(name,context,next){
      if(name==='node:child_process')return {url:'data:text/javascript,'+encodeURIComponent(${JSON.stringify(log + "export const spawnSync=(executable,args)=>{record(['tests',...args]);return {status:0};};")}),shortCircuit:true};
      return next(name,context);
    },load(url,context,next){return modules[url]===undefined?next(url,context):{format:'module',source:modules[url],shortCircuit:true};}});`);
  for (const [script, args, expected] of [
    ['build.mjs', [], [['backend', null], ['sdk'], ['web']]],
    ['build-deployment.mjs', ['--all'], [['backend', null], ['web']]],
    ['build-development.mjs', ['--component', 'chat-bridge'], [['backend', 'chat-bridge'], ['sdk'], ['tests', 'tools/testing/test.mjs', '--scope', 'chat']]],
  ]) {
    writeFileSync(record, '');
    const result = spawnSync(process.execPath, ['--import', pathToFileURL(hook).href, resolve('tools/build', script), ...args],
      { cwd: f.root, windowsHide: true, encoding: 'utf8', timeout: 30000 });
    assert.equal(result.status, 0, result.stderr + result.stdout);
    assert.deepEqual(readFileSync(record, 'utf8').trim().split('\n').map(line => JSON.parse(line)), expected);
  }
});
