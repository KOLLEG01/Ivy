import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, open, readFile, readdir, realpath, rm, stat } from 'node:fs/promises';
import { dirname, isAbsolute, join, relative, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
import { DatabaseSync } from 'node:sqlite';

const keys = ['distribution', 'native', 'native-sha256', 'candidate-commit', 'evidence'];
const { values } = parseArgs({ options: Object.fromEntries(keys.map(key => [key, { type: 'string' }])) });
for (const key of keys) assert.ok(values[key], `Missing --${key}`);
assert.equal(process.platform, 'win32', 'This candidate qualification is Windows only');
assert.equal(process.arch, 'x64');
assert.match(process.version, /^v24\.(?:18|19|[2-9][0-9])\./);
assert.match(values['native-sha256'], /^[a-f0-9]{64}$/);
assert.match(values['candidate-commit'], /^[a-f0-9]{40}$/);
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const distribution = await realpath(resolve(values.distribution));
const executable = await realpath(resolve(values.native));
assert.equal(digest(await readFile(executable)), values['native-sha256'], 'Native candidate bytes changed');
const load = name => import(pathToFileURL(join(distribution, 'dist', name)).href);
const { inspectStoppedStorage } = await load('services/hive/src/inspect-storage.js');
const { schemaSql } = await load('services/hive/src/storage-schema.js');
const { validateHost } = await load('packages/contracts/src/host-validation.js');
const { IvyError } = await load('packages/contracts/src/errors.js');
const { privateDirectory } = await load('packages/host-runtime/src/backup-files.js');
const evidence = resolve(values.evidence);
await privateDirectory(evidence);
const record = async (name, value) => {
  assert.match(name, /^[a-z0-9-]+$/);
  const file = await open(join(evidence, `${name}.json`), 'wx', 0o600);
  try { await file.writeFile(JSON.stringify(value, null, 2) + '\n'); await file.sync(); } finally { await file.close(); }
};
const fixture = await realpath(await mkdtemp(join(evidence, 'fixture-')));
const owned = path => {
  const local = relative(fixture, resolve(path));
  assert.ok(local !== '..' && !local.startsWith('..\\') && !local.startsWith('../') && !isAbsolute(local));
  return resolve(path);
};
await mkdir(owned(join(fixture, 'home')));
const config = {
  schemaVersion: 1, hostId: 'fixture-host', instanceId: 'fixture-hive', serviceNodeId: 'fixture.hive', componentId: 'hive',
  publicBaseUrl: 'https://fixture.invalid/ivy', dataRoot: owned(join(fixture, 'data')),
  artifactRoot: owned(join(fixture, 'artifact')), buildId: 'sha256:' + 'a'.repeat(64), version: '1.0.0', settings: {},
};
validateHost('InstanceConfig', config);
await mkdir(config.dataRoot);
await mkdir(config.artifactRoot);
const filename = owned(join(config.dataRoot, 'hive.sqlite'));
const snapshot = async () => {
  const files = [];
  for (const name of (await readdir(config.dataRoot)).sort()) {
    const path = owned(join(config.dataRoot, name));
    assert.ok((await stat(path)).isFile(), 'Unexpected fixture directory');
    const bytes = await readFile(path);
    files.push({ name, bytes: bytes.length, sha256: digest(bytes) });
  }
  return files;
};
const env = { ...process.env };
for (const key of Object.keys(env)) if (/^IVY_/.test(key)) delete env[key];
for (const key of ['HOME', 'USERPROFILE', 'APPDATA', 'LOCALAPPDATA', 'TEMP', 'TMP']) env[key] = owned(join(fixture, 'home'));
const runNative = args => new Promise((yes, no) => {
  const child = spawn(executable, args, { cwd: fixture, env, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = '', stderr = '', expired = false, oversized = false;
  const timer = setTimeout(() => { expired = true; child.kill(); }, 10000);
  child.stdout.on('data', chunk => { stdout += chunk; if (stdout.length > 65536) { oversized = true; child.kill(); } });
  child.stderr.on('data', chunk => { stderr += chunk; if (stderr.length > 65536) { oversized = true; child.kill(); } });
  child.stdin.on('error', () => {}); // The captured exit/streams remain authoritative on early rejection.
  child.once('error', error => { clearTimeout(timer); no(error); });
  child.once('close', (exitCode, signal) => { clearTimeout(timer); yes({ exitCode, signal, stdout, stderr, expired, oversized }); });
  child.stdin.end(JSON.stringify(config));
});
const oracle = args => {
  try {
    const index = args.indexOf('--contracts');
    const requirements = index < 0 ? [] : JSON.parse(args[index + 1]);
    assert.ok(Array.isArray(requirements));
    const result = inspectStoppedStorage(filename, requirements);
    validateHost('StorageInspection', result);
    return { exitCode: 0, stdout: JSON.stringify(result) + '\n', stderr: '' };
  } catch (error) {
    const code = error.code === 'ERR_ASSERTION' ? 'invalid_arguments' : IvyError.from(error).code;
    return { exitCode: 1, stdout: '', stderr: JSON.stringify({ code, message: 'Hive storage inspection failed without mutation.' }) + '\n' };
  }
};
const results = [];
async function check(id, args, expected) {
  const before = await snapshot();
  await record(`${id}-request`, { candidateCommit: values['candidate-commit'], nativeSha256: values['native-sha256'], config, args, before });
  const reference = oracle(args);
  assert.deepEqual(await snapshot(), before, 'Reference mutated fixture storage');
  const actual = await runNative(args);
  await record(`${id}-observed`, { reference, actual, after: await snapshot() });
  assert.equal(actual.expired, false, 'Native inspector expired');
  assert.equal(actual.oversized, false, 'Native inspector output limit');
  assert.equal(actual.signal, null);
  assert.deepEqual({ exitCode: actual.exitCode, stdout: actual.stdout, stderr: actual.stderr }, reference);
  const content = JSON.parse(actual.exitCode === 0 ? actual.stdout : actual.stderr);
  assert.deepEqual(content, expected, 'Independent expected inspection outcome');
  assert.deepEqual(await snapshot(), before, 'Native inspector mutated fixture storage');
  results.push({ id, passed: true });
}
const args = requirements => ['--contracts', JSON.stringify(requirements)];
const success = (exists, contracts) => ({ schemaVersion: 1, exists, format: exists ? 4 : 0, contracts });
const failure = code => ({ code, message: 'Hive storage inspection failed without mutation.' });
let db;
try {
  await check('absent-default', [], success(false, []));
  await check('absent-requested', args([{ key: 'fixture' }]), success(false, [{ key: 'fixture', versions: [] }]));
  db = new DatabaseSync(filename);
  db.exec(schemaSql);
  db.exec('PRAGMA user_version=4');
  db.prepare('INSERT INTO contract_families VALUES(?,?,?,?)').run('fixture', 'fixture-owner', 'application/json', 'retain');
  for (const version of ['1.0.0', '2.0.0', '3.0.0']) db.prepare('INSERT INTO contracts VALUES(?,?,?)').run('fixture', version, '{}');
  // Synthetic retained revisions under the original complete format-4 schema.
  db.exec('BEGIN'); // objects/current_revision and revisions/object_id are mutually dependent.
  for (const [id, parent, current, archived] of [['root', null, 2, 0], ['child', 'root', 1, 0], ['archived', 'root', 1, 1]]) {
    db.prepare(`INSERT INTO objects(id,parent_id,owner_object_id,name,path,position,icon,depth,contract_key,current_revision,archived_at,effective_archive,created_at,updated_at)
      VALUES(?,?,NULL,?,?,0,NULL,?,'fixture',?,NULL,?,'fixture-time','fixture-time')`).run(id, parent, id, '/' + id, parent ? 2 : 1, current, archived);
    for (let revision = 1; revision <= current; revision++) {
      const version = id === 'archived' ? '3.0.0' : id === 'root' && revision === 1 ? '1.0.0' : '2.0.0';
      db.prepare(`INSERT INTO revisions(object_id,revision,contract_key,contract_version,encoding,content,content_hash,byte_length,created_at,references_json)
        VALUES(?,?,'fixture',?,'json','{}',?,2,'fixture-time','[]')`).run(id, revision, version, 'sha256:' + digest('{}'));
    }
  }
  db.exec('COMMIT');
  db.close(); db = null;
  await check('retained-all', args([{ key: 'fixture' }]), success(true, [{ key: 'fixture', versions: ['1.0.0', '2.0.0', '3.0.0'] }]));
  const current = { roots: null, history: 'current', includeArchived: false, references: [] };
  await check('current-unarchived', args([{ key: 'fixture', readScope: current }]), success(true, [{ key: 'fixture', versions: ['2.0.0'] }]));
  await check('subtree-history', args([{ key: 'fixture', readScope: { ...current, roots: ['root'], history: 'all' } }]), success(true, [{ key: 'fixture', versions: ['1.0.0', '2.0.0'] }]));
  await check('pinned-archived', args([{ key: 'fixture', readScope: { ...current, references: [{ objectId: 'archived', revision: 1 }] } }]), success(true, [{ key: 'fixture', versions: ['2.0.0', '3.0.0'] }]));
  await check('missing-root', args([{ key: 'fixture', readScope: { ...current, roots: ['absent'] } }]), failure('not_found'));
  await check('duplicate-keys', args([{ key: 'fixture' }, { key: 'fixture' }]), failure('invalid_arguments'));
  await check('first-contracts-flag', [...args([]), '--contracts', 'not-json'], success(true, []));
  await check('invalid-contracts-json', ['--contracts', '{'], failure('internal_error'));
  await check('invalid-contracts-array', ['--contracts', '{}'], failure('invalid_arguments'));
  db = new DatabaseSync(filename); db.exec('BEGIN EXCLUSIVE');
  await check('exclusive-writer', args([{ key: 'fixture' }]), failure('internal_error'));
  db.exec('ROLLBACK; PRAGMA user_version=3'); db.close(); db = null;
  await check('unsupported-format', args([]), failure('unsupported_storage_format'));
  await record('result', { passed: true, results, platform: { os: process.platform, arch: process.arch, node: process.version }, nativeSha256: values['native-sha256'], candidateCommit: values['candidate-commit'], scope: 'admitted offline Hive inspector; generic Host admission, release packaging and live service lifecycle excluded' });
  console.log(JSON.stringify({ passed: true, cases: results.length, evidence }));
} finally {
  db?.close();
  assert.equal(dirname(await realpath(fixture)), await realpath(evidence));
  await rm(fixture, { recursive: true, force: true });
}
