import test from 'node:test';
import assert from 'node:assert/strict';
import { cpSync, mkdtempSync, mkdirSync, writeFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { cachedBuild, compilerCache, fingerprint } from '../tools/build/build-cache.mjs';

test('build cache checks source changes, deletions, output integrity, force and failed builds', async t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-cache-unit-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const input = join(root, 'input'), output = join(root, 'output'), stamp = join(root, 'cache.json');
  mkdirSync(input); writeFileSync(join(input, 'a'), 'one');
  let runs = 0;
  const options = { stamp, inputs: [input], outputs: [output] };
  const build = async () => { runs++; writeFileSync(output, 'built'); };
  assert.equal(await cachedBuild(options, build), true);
  assert.equal(await cachedBuild(options, build), false);
  writeFileSync(join(input, 'a'), 'two'); await cachedBuild(options, build);
  writeFileSync(join(input, 'b'), 'added'); await cachedBuild(options, build);
  rmSync(join(input, 'b')); await cachedBuild(options, build);
  writeFileSync(output, 'damaged'); await cachedBuild(options, build);
  rmSync(output); await cachedBuild(options, build);
  await cachedBuild({ ...options, force: true }, build);
  assert.equal(runs, 7);
  await assert.rejects(cachedBuild({ ...options, force: true }, async () => { throw Error('failed'); }));
  assert.equal(existsSync(stamp), false);
  assert.equal(await cachedBuild(options, build), true);
  await cachedBuild({ ...options, force: true }, async () => { await build(); writeFileSync(join(input, 'a'), 'changed during build'); });
  assert.equal(existsSync(stamp), false);
});

test('compiler cache resets TypeScript state when emitted files disappear', t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-cache-unit-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const info = join(root, 'buildinfo'), output = join(root, 'output.js');
  const finish = compilerCache(info, [output]);
  writeFileSync(info, 'compiler state'); writeFileSync(output, 'code'); finish();
  compilerCache(info, [output])(); assert.equal(existsSync(info), true);
  rmSync(output); compilerCache(info, [output]); assert.equal(existsSync(info), false);
});

test('relative fingerprints identify equal immutable inputs across snapshot roots', t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-cache-unit-')), first = join(root, 'first'), second = join(root, 'second');
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(first, 'nested'), { recursive: true }); writeFileSync(join(first, 'nested', 'input'), 'same bytes');
  cpSync(first, second, { recursive: true });
  assert.equal(fingerprint([first], { compiler: 'fixed' }, first), fingerprint([second], { compiler: 'fixed' }, second));
  writeFileSync(join(second, 'nested', 'input'), 'changed');
  assert.notEqual(fingerprint([first], { compiler: 'fixed' }, first), fingerprint([second], { compiler: 'fixed' }, second));
});
