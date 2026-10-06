import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import { dirname, isAbsolute, join, resolve } from 'node:path';

const reference = Object.freeze({
  url: 'https://www.etsi.org/deliver/etsi_ts/126400_126499/126443/19.00.00_60/ts_126443v190000p0.zip',
  sha256: 'f3e0928f917fe67a620a37578757b0db00e0ca216481a96a99ea8ff5e3b8271b', nested: '26443-i00-ANSI-C_source_code.zip',
});
const hash = value => createHash('sha256').update(value).digest('hex');

// An explicitly selected local build cache is trusted like the local compiler. Hash checks bind
// reuse to these exact authored/reference inputs and output bytes; they are not a signature.
export async function readPhoneEvsBuild(root, path) {
  assert.ok(isAbsolute(path), 'EVS build report must be an absolute local path.');
  const report = JSON.parse(await readFile(path, 'utf8'));
  assert.equal(report.schemaVersion, 1); assert.deepEqual(report.reference, reference);
  assert.equal(report.compilerTarget, 'x86_64-w64-mingw32');
  assert.ok(typeof report.compilerVersion === 'string' && report.compilerVersion.length > 0);
  assert.ok(isAbsolute(report.work));
  assert.deepEqual(report.flags, ['-std=c99', '-O3', '-DNDEBUG', '-DRELEASE', '-D__unix__', '-fno-strict-aliasing',
    ...['lib_com', 'lib_enc', 'lib_dec'].map(name => '-I' + join(report.work, 'source/c-code', name))]);
  for (const [key, file] of Object.entries({ adapter: 'phone_evs.c', header: 'phone_evs.h', test: 'phone_evs_test.c' }))
    assert.equal(hash(await readFile(join(root, 'services/phone-bridge/native/evs', file))), report.authoredInputs[key], 'EVS authored input changed: ' + file);
  assert.equal(report.files, 369, 'Pinned EVS translation-unit set changed.');
  assert.equal(resolve(report.library), resolve(report.work, 'ivy_phone_evs.dll'));
  assert.equal(resolve(report.testExecutable), resolve(report.work, 'phone_evs_test.exe'));
  const library = await readFile(report.library);
  assert.equal(hash(library), report.librarySha256, 'EVS library differs from original build evidence.');
  assert.equal(hash(await readFile(report.testExecutable)), report.testSha256, 'EVS test executable differs from original build evidence.');
  return { report, library };
}

export async function preparePhoneEvs(root, reportPath = process.env.IVY_EVS_BUILD_REPORT) {
  // A source-keyed pointer lets prepare/check/publish reuse one dependency build within a
  // source snapshot. Different C inputs or builder recipe never select the same pointer.
  const inputs = await Promise.all(['services/phone-bridge/native/evs/phone_evs.c', 'services/phone-bridge/native/evs/phone_evs.h', 'services/phone-bridge/native/evs/phone_evs_test.c',
    'tools/build/build-phone-evs.mjs'].map(async path => [path, hash(await readFile(join(root, path)))]));
  const input = hash(JSON.stringify(inputs));
  const pointer = join(root, '.local/phone-evs-cache', input + '.json');
  const sharedRoot = process.env.IVY_BUILD_CACHE_ROOT;
  assert.ok(sharedRoot === undefined || isAbsolute(sharedRoot), 'Selected EVS build cache must be absolute.');
  const shared = sharedRoot && !process.env.IVY_TEST_TEMP ? join(sharedRoot, 'phone-evs', input) : null;
  if (!reportPath) {
    try { reportPath = JSON.parse(await readFile(pointer, 'utf8')).buildReport; assert.ok(typeof reportPath === 'string'); }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
  }
  if (shared) {
    try {
      const candidate = join(shared, 'build.json');
      await readPhoneEvsBuild(root, candidate); reportPath = candidate;
    } catch (error) {
      if (error.code !== 'ENOENT') await rm(shared, { recursive: true, force: true });
    }
  }
  if (!reportPath) {
    const output = await new Promise((resolveOutput, reject) => {
      const child = spawn(process.execPath, [join(root, 'tools/build/build-phone-evs.mjs')],
        { cwd: root, windowsHide: true, stdio: ['ignore', 'pipe', 'inherit'] });
      let stdout = '';
      child.stdout.setEncoding('utf8');
      child.stdout.on('data', value => { stdout += value; if (stdout.length > 1024 * 1024) child.kill(); });
      child.once('error', reject);
      child.once('close', code => code === 0 ? resolveOutput(stdout) : reject(new Error('EVS dependency build failed: ' + code)));
    });
    const built = JSON.parse(output.trim()); reportPath = join(built.work, 'build.json');
  }
  if (shared && resolve(dirname(reportPath)) !== resolve(shared)) {
    const validated = await readPhoneEvsBuild(root, reportPath);
    const incoming = shared + '.incoming-' + randomUUID();
    await mkdir(incoming, { recursive: true });
    try {
      await writeFile(join(incoming, 'ivy_phone_evs.dll'), validated.library, { flag: 'wx' });
      await writeFile(join(incoming, 'phone_evs_test.exe'), await readFile(validated.report.testExecutable), { flag: 'wx' });
      const cached = { ...validated.report, work: shared, library: join(shared, 'ivy_phone_evs.dll'),
        testExecutable: join(shared, 'phone_evs_test.exe'),
        flags: ['-std=c99', '-O3', '-DNDEBUG', '-DRELEASE', '-D__unix__', '-fno-strict-aliasing',
          ...['lib_com', 'lib_enc', 'lib_dec'].map(name => '-I' + join(shared, 'source/c-code', name))] };
      await writeFile(join(incoming, 'build.json'), JSON.stringify(cached, null, 2) + '\n', { flag: 'wx' });
      await mkdir(dirname(shared), { recursive: true });
      try {
        await rename(incoming, shared); reportPath = join(shared, 'build.json');
      } catch (error) {
        if (!['EEXIST', 'ENOTEMPTY', 'EPERM'].includes(error.code)) throw error;
        reportPath = join(shared, 'build.json');
      }
    } finally { await rm(incoming, { recursive: true, force: true }); }
  }
  const built = await readPhoneEvsBuild(root, reportPath);
  if (!process.env.IVY_TEST_TEMP) {
    await mkdir(dirname(pointer), { recursive: true });
    try { await writeFile(pointer, JSON.stringify({ buildReport: reportPath }) + '\n', { flag: 'wx' }); }
    catch (error) { if (error.code !== 'EEXIST') throw error; }
  }
  return built;
}

export async function installPhoneEvs(root, destination, built, { replace = false } = {}) {
  // Revalidate before copying, then write the validated bytes (not a second mutable source read).
  const checked = await readPhoneEvsBuild(root, join(built.report.work, 'build.json'));
  assert.equal(checked.report.librarySha256, built.report.librarySha256, 'Selected EVS dependency changed before packaging.');
  await mkdir(destination, { recursive: true });
  const libraryPath = join(destination, 'ivy_phone_evs.dll');
  const noticePath = join(destination, 'licenses/EVS-reference.md');
  const provenancePath = join(destination, 'evs-build.json');
  if (replace) for (const path of [libraryPath, noticePath, provenancePath]) await rm(path, { force: true });
  await writeFile(libraryPath, checked.library, { flag: 'wx' });
  await mkdir(join(destination, 'licenses'), { recursive: true });
  await writeFile(noticePath, await readFile(join(root, 'services/phone-bridge/native/evs/NOTICE.md')), { flag: 'wx' });
  const { compilerVersion, compilerTarget, authoredInputs, librarySha256, testSha256 } = checked.report;
  await writeFile(provenancePath, JSON.stringify({ schemaVersion: 1, abi: 1, reference,
    compilerVersion, compilerTarget, authoredInputs, librarySha256, testSha256 }, null, 2) + '\n', { flag: 'wx' });
}
