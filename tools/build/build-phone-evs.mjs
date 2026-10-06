import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import { isAbsolute, join } from 'node:path';
import { fileURLToPath } from 'node:url';

// Separate reproducible dependency build. No tests, package activation or runtime mutation.
// Only the pinned archive is extracted, and every output belongs to this fresh build directory.
assert.equal(process.platform, 'win32'); assert.equal(process.arch, 'x64');
assert.equal(process.argv.length, 2, 'Configuration uses IVY_EVS_CC / IVY_EVS_JOBS / IVY_EVS_ARCHIVE only.');
const root = fileURLToPath(new URL('../../', import.meta.url));
const work = join(process.env.IVY_TEST_TEMP ?? join(root, '.local'), 'phone-evs-' + randomUUID());
await mkdir(work, { recursive: true });
const compiler = process.env.IVY_EVS_CC ?? 'gcc';
const jobs = Number(process.env.IVY_EVS_JOBS ?? 4);
assert.ok(Number.isInteger(jobs) && jobs >= 1 && jobs <= 32);
const reference = { url: 'https://www.etsi.org/deliver/etsi_ts/126400_126499/126443/19.00.00_60/ts_126443v190000p0.zip',
  sha256: 'f3e0928f917fe67a620a37578757b0db00e0ca216481a96a99ea8ff5e3b8271b',
  nested: '26443-i00-ANSI-C_source_code.zip' };
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
async function run(label, executable, args, timeout = 120_000) {
  const started = Date.now();
  const result = await new Promise((resolveResult, reject) => {
    const child = spawn(executable, args, { cwd: work, windowsHide: true, timeout, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '', stderr = '';
    child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
    child.stdout.on('data', value => { stdout += value; if (stdout.length > 4 * 1024 * 1024) child.kill(); });
    child.stderr.on('data', value => { stderr += value; if (stderr.length > 4 * 1024 * 1024) child.kill(); });
    child.once('error', reject);
    child.once('close', (code, signal) => resolveResult({ executable, args, code, signal, stdout, stderr, durationMs: Date.now() - started }));
  });
  if (result.code !== 0) process.stderr.write(`${label}: ${result.stderr ?? ''}\n${result.stdout ?? ''}\n`);
  assert.equal(result.code, 0, `${label} failed; original output: ${work}`);
  return result.stdout.trim();
}
const compilerVersion = await run('compiler-version', compiler, ['--version']);
const compilerTarget = await run('compiler-target', compiler, ['-dumpmachine']);
assert.equal(compilerTarget, 'x86_64-w64-mingw32', 'Windows x64 MinGW compiler required.');
let archive;
if (process.env.IVY_EVS_ARCHIVE) {
  assert.ok(isAbsolute(process.env.IVY_EVS_ARCHIVE), 'Cached source archive must be an absolute path.');
  archive = await readFile(process.env.IVY_EVS_ARCHIVE);
} else {
  const response = await fetch(reference.url, { signal: AbortSignal.timeout(120_000) });
  assert.ok(response.ok, 'Pinned ETSI source download failed.');
  assert.ok(Number(response.headers.get('content-length') ?? 0) <= 16 * 1024 * 1024);
  const chunks = []; let size = 0;
  for await (const chunk of response.body) {
    size += chunk.length; assert.ok(size <= 16 * 1024 * 1024, 'ETSI archive exceeds bound.'); chunks.push(chunk);
  }
  archive = Buffer.concat(chunks);
}
assert.ok(archive.length <= 16 * 1024 * 1024); assert.equal(hash(archive), reference.sha256);
const outer = join(work, 'outer'), source = join(work, 'source'), objects = join(work, 'objects');
for (const path of [outer, source, objects]) await mkdir(path);
const archivePath = join(work, 'reference.zip'); await writeFile(archivePath, archive);
const tar = join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'tar.exe');
await run('extract-outer', tar, ['-xf', archivePath, '-C', outer]);
await run('extract-source', tar, ['-xf', join(outer, reference.nested), '-C', source]);
const code = join(source, 'c-code'), folders = ['lib_com', 'lib_enc', 'lib_dec'];
const adapter = join(root, 'services/phone-bridge/native/evs/phone_evs.c'), test = join(root, 'services/phone-bridge/native/evs/phone_evs_test.c');
// Snapshot authored inputs once. Root can develop the next batch without altering this build.
const adapterBytes = await readFile(adapter), testBytes = await readFile(test), headerBytes = await readFile(join(root, 'services/phone-bridge/native/evs/phone_evs.h'));
const localAdapter = join(work, 'phone_evs.c'), localTest = join(work, 'phone_evs_test.c');
await writeFile(localAdapter, adapterBytes); await writeFile(localTest, testBytes);
await writeFile(join(work, 'phone_evs.h'), headerBytes);
const files = [localAdapter];
for (const folder of folders) for (const name of (await readdir(join(code, folder))).sort()) {
  if (name.endsWith('.c') && !['encoder.c', 'decoder.c', 'voip_client.c'].includes(name)) files.push(join(code, folder, name));
}
assert.ok(files.length > 300, 'Pinned reference source set is incomplete.');
const flags = ['-std=c99', '-O3', '-DNDEBUG', '-DRELEASE', '-D__unix__', '-fno-strict-aliasing', ...folders.map(name => '-I' + join(code, name))];
const outputs = files.map((_, index) => join(objects, `${index}.o`));
let next = 0, failure;
await Promise.all(Array.from({ length: jobs }, async () => {
  while (!failure && next < files.length) {
    const index = next++;
    try { await run('compile-' + index, compiler, [...flags, '-c', files[index], '-o', outputs[index]]); }
    catch (error) { failure ??= error; }
  }
}));
if (failure) throw failure;
const responseFile = join(work, 'objects.rsp');
await writeFile(responseFile, outputs.map(path => '"' + path.replaceAll('\\', '/') + '"').join('\n') + '\n');
const library = join(work, 'ivy_phone_evs.dll'), imports = join(work, 'libivy_phone_evs.a');
await run('link-library', compiler, ['-shared', '-static-libgcc', '@' + responseFile, '-Wl,--out-implib,' + imports, '-o', library, '-lm', '-lws2_32']);
const executable = join(work, 'phone_evs_test.exe');
await run('link-test', compiler, ['-std=c99', '-O2', '-static-libgcc', localTest, imports, '-o', executable, '-lm']);
const report = { schemaVersion: 1, reference, compiler, compilerVersion, compilerTarget, flags, jobs,
  authoredInputs: { adapter: hash(adapterBytes), test: hash(testBytes), header: hash(headerBytes) }, files: files.length, work,
  library, librarySha256: hash(await readFile(library)), testExecutable: executable,
  testSha256: hash(await readFile(executable)), testsRun: false };
await writeFile(join(work, 'build.json'), JSON.stringify(report, null, 2) + '\n');
process.stdout.write(JSON.stringify(report) + '\n');
