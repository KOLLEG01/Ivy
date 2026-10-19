import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, rm, stat, symlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { collectInstallationReleases } from '../tools/operations/collect-installation-releases.mjs';

test('standalone release collection preserves current, unfinished and unrelated directories', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-installation-retention-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const releases = join(root, 'releases'); await mkdir(releases);
  const builds = ['1', '2', '3', '4', '5', '6'].map(value => 'sha256:' + value.repeat(64));
  for (const [index, buildId] of builds.entries()) {
    const directory = join(releases, buildId.slice(7)); await mkdir(join(directory, 'artifact'), { recursive: true });
    await writeFile(join(directory, 'component.json'), JSON.stringify({ schemaVersion: 1, componentId: index === 5 ? 'other' : 'fixture', buildId }));
  }
  const outside = join(root, 'outside'); await mkdir(outside); await writeFile(join(outside, 'keep'), 'data');
  await symlink(outside, join(releases, '7'.repeat(64)), process.platform === 'win32' ? 'junction' : 'dir');
  await symlink(outside, join(releases, builds[3].slice(7), 'artifact', 'external'), process.platform === 'win32' ? 'junction' : 'dir');
  const protectedBuilds = new Set([builds[0], builds[2]]), pending = new Set([builds[2]]);
  await assert.rejects(collectInstallationReleases(releases, 'fixture', ''));
  assert.deepEqual((await collectInstallationReleases(releases, 'fixture', builds[0], pending)).sort(), [builds[1], builds[3], builds[4]]);
  for (const buildId of [...protectedBuilds, builds[5]]) assert.ok(await stat(join(releases, buildId.slice(7))));
  assert.equal(await readFile(join(outside, 'keep'), 'utf8'), 'data');
  assert.deepEqual(await collectInstallationReleases(releases, 'fixture', builds[0], pending), []);
});
