import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, mkdir, writeFile, rm, symlink } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { artifactFiles } from '../packages/host-runtime/src/artifact.js';

test('artifact directory batches preserve all nested files and deterministic paths', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-artifact-directories-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const expected = [];
  for (let index = 0; index < 13; index++) {
    const path = `branch-${index}/deeper/leaf.txt`, content = `distinct bytes ${index}`;
    await mkdir(join(root, `branch-${index}/deeper/empty`), { recursive: true });
    await writeFile(join(root, path), content);
    expected.push({ path, hash: 'sha256:' + createHash('sha256').update(content).digest('hex') });
  }
  expected.sort((a, b) => a.path < b.path ? -1 : 1);
  assert.deepEqual((await artifactFiles(root)).map(({ path, hash }) => ({ path, hash })), expected);
});

test('artifact directory batches reject a nested link and drain before returning', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-artifact-directory-link-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const release = join(root, 'release'), outside = join(root, 'outside');
  await mkdir(outside);
  await writeFile(join(outside, 'private'), 'outside');
  for (let index = 0; index < 9; index++) await mkdir(join(release, `branch-${index}/nested`), { recursive: true });
  await symlink(outside, join(release, 'branch-7/nested/link'), process.platform === 'win32' ? 'junction' : 'dir');
  await assert.rejects(artifactFiles(release), { code: 'artifact_changed' });
  await rm(join(release, 'branch-7/nested/link'));
  assert.deepEqual(await artifactFiles(release), []);
});
