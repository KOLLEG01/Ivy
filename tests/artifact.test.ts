import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, mkdir, writeFile, rm, stat, symlink } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { artifactFiles, fileHash } from '../packages/host-runtime/src/artifact.js';

test('artifact hashes cover full blocks, partial tails and empty files across concurrent scans', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-artifact-hash-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const files = new Map<string, Buffer>([
    ['a-large', Buffer.alloc(3 * 65536 + 137, 0xa5)],
    ['b-empty', Buffer.alloc(0)],
    ['c-short', Buffer.from('A shorter file must not hash the previous buffer tail.')],
    ['nested/d-block', Buffer.alloc(65536, 0x3b)],
    ['nested/e-tail', Buffer.from([0, 255, 1])],
  ]);
  // More than one admission wave, with distinct repeated buffer tails and empty files.
  for (let index = 0; index < 17; index++) files.set('wave-' + String(index).padStart(2, '0'), Buffer.alloc(index % 3 === 0 ? 0 : 65536 + index, index));
  await mkdir(join(root, 'nested'));
  for (const [name, bytes] of files) await writeFile(join(root, name), bytes);
  const expected = await Promise.all([...files].map(async ([path, bytes]) => ({ path, bytes: bytes.length,
    hash: 'sha256:' + createHash('sha256').update(bytes).digest('hex'), mode: (await stat(join(root, path))).mode & 0o777 })));
  const results = await Promise.all([artifactFiles(root), artifactFiles(root), ...[...files.keys()].map(path => fileHash(join(root, path)))]);
  assert.deepEqual(results[0], expected); assert.deepEqual(results[1], expected);
  assert.deepEqual(results.slice(2), expected.map(value => value.hash));
  await assert.rejects(fileHash(join(root, 'missing')), { code: 'ENOENT' });
  const changed = Buffer.from(files.get('a-large')!); changed[changed.length - 1] = changed[changed.length - 1]! ^ 0xff;
  await writeFile(join(root, 'a-large'), changed);
  assert.notEqual(await fileHash(join(root, 'a-large')), expected[0]!.hash);
  await rm(join(root, 'a-large')); // No file handle survives a completed hash, including on Windows.
  await rm(root, { recursive: true }); // Every completed traversal also releases all worker handles.
});

test('artifact traversal rejects linked directories before hashing outside data and still handles an empty release', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-artifact-link-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const release = join(root, 'release'), outside = join(root, 'outside');
  await mkdir(release); await mkdir(outside);
  assert.deepEqual(await artifactFiles(release), []);
  await writeFile(join(outside, 'unowned'), 'This is not release data.');
  await writeFile(join(release, 'owned'), 'A regular release file.');
  await symlink(outside, join(release, 'linked'), process.platform === 'win32' ? 'junction' : 'dir');
  await assert.rejects(artifactFiles(release), { code: 'artifact_changed' });
  await rm(join(release, 'linked'));
  assert.equal((await artifactFiles(release)).length, 1);
  assert.equal(await fileHash(join(outside, 'unowned')), 'sha256:' + createHash('sha256').update('This is not release data.').digest('hex'));
});
