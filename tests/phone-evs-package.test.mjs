import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, mkdir, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { readPhoneEvsBuild, installPhoneEvs, preparePhoneEvs } from '../tools/package/phone-evs-package.mjs';
const hash = value => createHash('sha256').update(value).digest('hex');

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), 'ivy-evs-package-')), work = join(root, 'build');
  await mkdir(join(root, 'services/phone-bridge/native/evs'), { recursive: true }); await mkdir(work);
  await mkdir(join(root, 'tools/build'), { recursive: true }); await writeFile(join(root, 'tools/build/build-phone-evs.mjs'), 'synthetic recipe; must never execute');
  const authoredInputs = {};
  for (const [key, file] of Object.entries({ adapter: 'phone_evs.c', header: 'phone_evs.h', test: 'phone_evs_test.c' })) {
    const bytes = Buffer.from('synthetic-authored-' + key); authoredInputs[key] = hash(bytes);
    await writeFile(join(root, 'services/phone-bridge/native/evs', file), bytes);
  }
  await writeFile(join(root, 'services/phone-bridge/native/evs/NOTICE.md'), 'synthetic dependency notice');
  const library = join(work, 'ivy_phone_evs.dll'), testExecutable = join(work, 'phone_evs_test.exe');
  await writeFile(library, 'synthetic-library'); await writeFile(testExecutable, 'synthetic-test');
  const report = { schemaVersion: 1, reference: {
    url: 'https://www.etsi.org/deliver/etsi_ts/126400_126499/126443/19.00.00_60/ts_126443v190000p0.zip',
    sha256: 'f3e0928f917fe67a620a37578757b0db00e0ca216481a96a99ea8ff5e3b8271b', nested: '26443-i00-ANSI-C_source_code.zip',
  }, compilerVersion: 'fixture', compilerTarget: 'x86_64-w64-mingw32', authoredInputs, files: 369, work, library, testExecutable,
  librarySha256: hash('synthetic-library'), testSha256: hash('synthetic-test'),
  flags: ['-std=c99', '-O3', '-DNDEBUG', '-DRELEASE', '-D__unix__', '-fno-strict-aliasing',
    ...['lib_com', 'lib_enc', 'lib_dec'].map(name => '-I' + join(work, 'source/c-code', name))] };
  const path = join(work, 'build.json'); const save = () => writeFile(path, JSON.stringify(report)); await save();
  return { root, work, report, path, save };
}

test('EVS package reuses exact local build bytes and retains portable provenance and notice', async () => {
  const f = await fixture(), built = await readPhoneEvsBuild(f.root, f.path), destination = join(f.root, 'published');
  await installPhoneEvs(f.root, destination, built);
  assert.equal(await readFile(join(destination, 'ivy_phone_evs.dll'), 'utf8'), 'synthetic-library');
  assert.equal(await readFile(join(destination, 'licenses/EVS-reference.md'), 'utf8'), 'synthetic dependency notice');
  const provenance = JSON.parse(await readFile(join(destination, 'evs-build.json'), 'utf8'));
  assert.equal(provenance.librarySha256, f.report.librarySha256); assert.equal(provenance.abi, 1);
  assert.equal(provenance.work, undefined); assert.equal(provenance.library, undefined);
  await assert.rejects(installPhoneEvs(f.root, destination, built), { code: 'EEXIST' });
});

test('EVS test package replaces stale generated files on an explicit refresh', async () => {
  const f = await fixture(), built = await readPhoneEvsBuild(f.root, f.path), destination = join(f.root, 'published');
  await mkdir(join(destination, 'licenses'), { recursive: true });
  await writeFile(join(destination, 'ivy_phone_evs.dll'), 'stale-library');
  await writeFile(join(destination, 'licenses/EVS-reference.md'), 'stale-notice');
  await writeFile(join(destination, 'evs-build.json'), 'stale-provenance');
  await installPhoneEvs(f.root, destination, built, { replace: true });
  assert.equal(await readFile(join(destination, 'ivy_phone_evs.dll'), 'utf8'), 'synthetic-library');
  assert.equal(await readFile(join(destination, 'licenses/EVS-reference.md'), 'utf8'), 'synthetic dependency notice');
  assert.equal(JSON.parse(await readFile(join(destination, 'evs-build.json'), 'utf8')).librarySha256, f.report.librarySha256);
});

test('EVS cache refuses drift in source, reference, recipe, architecture or executable bytes', async () => {
  const mutations = [
    async f => writeFile(join(f.root, 'services/phone-bridge/native/evs/phone_evs.c'), 'changed adapter'),
    async f => { f.report.reference.sha256 = '0'.repeat(64); await f.save(); },
    async f => { f.report.flags[1] = '-O0'; await f.save(); },
    async f => { f.report.compilerTarget = 'i686-w64-mingw32'; await f.save(); },
    async f => writeFile(f.report.library, 'changed library'),
    async f => writeFile(f.report.testExecutable, 'changed test'),
    async f => { f.report.library = join(f.work, 'elsewhere.dll'); await f.save(); },
  ];
  for (const mutate of mutations) {
    const f = await fixture(); await mutate(f); await assert.rejects(readPhoneEvsBuild(f.root, f.path));
  }
});

test('EVS copy revalidates original build when bytes change after selection', async () => {
  const f = await fixture(), built = await readPhoneEvsBuild(f.root, f.path);
  await writeFile(f.report.library, 'changed between selection and copy');
  await assert.rejects(installPhoneEvs(f.root, join(f.root, 'published'), built));
});

test('EVS prepare/check/publish reuse one source-keyed dependency without executing a compiler', async () => {
  const f = await fixture();
  const previousTemp = process.env.IVY_TEST_TEMP;
  delete process.env.IVY_TEST_TEMP;
  try {
    const selected = await preparePhoneEvs(f.root, f.path);
    const reused = await preparePhoneEvs(f.root, null);
    assert.deepEqual(reused.report, selected.report);
    await writeFile(f.report.library, 'corrupt retained library');
    await assert.rejects(preparePhoneEvs(f.root, null), /differs from original build evidence/);
  } finally {
    if (previousTemp === undefined) delete process.env.IVY_TEST_TEMP;
    else process.env.IVY_TEST_TEMP = previousTemp;
  }
});

test('EVS dependency cache survives a fresh deployment source snapshot', async () => {
  const first = await fixture(), second = await fixture(), cache = join(first.root, 'shared-cache');
  const previousCache = process.env.IVY_BUILD_CACHE_ROOT, previousTemp = process.env.IVY_TEST_TEMP;
  process.env.IVY_BUILD_CACHE_ROOT = cache; delete process.env.IVY_TEST_TEMP;
  try {
    const selected = await preparePhoneEvs(first.root, first.path);
    const reused = await preparePhoneEvs(second.root, null);
    assert.equal(reused.report.librarySha256, selected.report.librarySha256);
    assert.equal(reused.report.work.startsWith(cache), true);
  } finally {
    if (previousCache === undefined) delete process.env.IVY_BUILD_CACHE_ROOT;
    else process.env.IVY_BUILD_CACHE_ROOT = previousCache;
    if (previousTemp === undefined) delete process.env.IVY_TEST_TEMP;
    else process.env.IVY_TEST_TEMP = previousTemp;
  }
});
