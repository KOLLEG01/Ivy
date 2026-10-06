import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, symlink } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { randomUUID } from 'node:crypto';
import { HiveStore } from '../dist/services/hive/src/store.js';
import { Objects } from '../dist/services/hive/src/objects.js';
import { canonical, digest } from '../dist/packages/contracts/src/canonical.js';
import { backupSettings, planSettingsRestore, restoreSettings } from '../dist/services/hive/src/settings-recovery.js';

const at = '2026-09-23T00:00:00.000Z';
function addObject(store, rootId, key, id, name, value) {
  const parent = store.get('SELECT path FROM objects WHERE id=?', rootId), content = canonical(value);
  store.transaction(() => {
    store.run('INSERT INTO objects(id,parent_id,owner_object_id,name,path,position,icon,depth,contract_key,current_revision,archived_at,effective_archive,created_at,updated_at) VALUES (?,?,NULL,?,?,0,NULL,2,?,1,NULL,0,?,?)',
      id, rootId, name, String(parent.path) + '/' + name, key, at, at);
    store.run('INSERT INTO revisions(object_id,revision,contract_key,contract_version,encoding,content,content_hash,byte_length,created_at,references_json) VALUES (?,1,?,\'1.0.0\',\'json\',?,?,?,?,\'{}\')',
      id, key, content, digest(content), Buffer.byteLength(content, 'utf8'), at);
    store.run('INSERT INTO object_fts(object_id,name,text) VALUES (?,?,?)', id, name, '');
  });
}

test('offline settings backup restores only missing allowlisted Objects and preserves current data', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'ivy-settings-recovery-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const sourcePath = join(directory, 'source.sqlite'), targetPath = join(directory, 'target.sqlite'),
    backupPath = join(directory, 'backup.sqlite'), safetyPath = join(directory, 'safety.sqlite');
  const rootId = randomUUID(), skillsId = randomUUID(), wikiId = randomUUID();
  const source = new HiveStore(sourcePath), root = new Objects(source);
  root.bootstrapScopeRoots([rootId], at);
  const contract = key => ({ key, version: '1.0.0', owner: { kind: 'agent' }, mediaType: 'application/json',
    retention: { objects: { mode: 'retain' }, revisions: { mode: 'current' } },
    jsonSchema: { type: 'object', additionalProperties: true }, specMarkdown: 'Settings recovery fixture.' });
  for (const key of ['agent/skills', 'wiki/page']) root.register(contract(key), { kind: 'agent' });
  addObject(source, rootId, 'agent/skills', skillsId, 'Skills', { schemaVersion: 1, selected: ['alpha'] });
  addObject(source, rootId, 'wiki/page', wikiId, 'Private page', { body: 'not a setting' });
  source.close();
  const target = new HiveStore(targetPath); new Objects(target).bootstrapScopeRoots([rootId], at); target.close();

  const backup = await backupSettings(sourcePath, backupPath);
  assert.equal(backup.total, 1);
  assert.match(backup.sourceHash, /^sha256:[a-f0-9]{64}$/);
  const cliPath = fileURLToPath(new URL('../dist/services/hive/src/settings-recovery.js', import.meta.url));
  const cliArgs = ['plan', backupPath, targetPath, backup.sourceHash];
  const direct = spawnSync(process.execPath, [cliPath, ...cliArgs], { encoding: 'utf8' });
  assert.equal(direct.status, 0, direct.stderr);
  assert.equal(JSON.parse(direct.stdout).result.restore, 1);
  const linkedPath = join(directory, 'settings-recovery-link.js');
  try {
    await symlink(cliPath, linkedPath);
    const linked = spawnSync(process.execPath, [linkedPath, ...cliArgs], { encoding: 'utf8' });
    assert.equal(linked.status, 0, linked.stderr);
    assert.equal(JSON.parse(linked.stdout).result.restore, 1);
  } catch (error) {
    if (error.code !== 'EPERM') throw error;
  }
  const preview = await planSettingsRestore(backupPath, targetPath, backup.sourceHash);
  assert.equal(preview.restore, 1); assert.equal(preview.conflicts, 0);
  const restored = await restoreSettings(backupPath, targetPath, backup.sourceHash, safetyPath);
  assert.equal(restored.restore, 1); assert.match(restored.safetyHash, /^sha256:[a-f0-9]{64}$/);
  const check = new HiveStore(targetPath);
  assert.equal(check.get('SELECT contract_key FROM objects WHERE id=?', skillsId).contract_key, 'agent/skills');
  assert.equal(check.get('SELECT id FROM objects WHERE id=?', wikiId), undefined);
  check.close();
  const repeated = await planSettingsRestore(backupPath, targetPath, backup.sourceHash);
  assert.equal(repeated.restore, 0); assert.equal(repeated.preserve, 1);
  await assert.rejects(planSettingsRestore(backupPath, targetPath, 'sha256:' + '0'.repeat(64)), { code: 'backup_hash_mismatch' });

  const conflictingPath = join(directory, 'conflicting.sqlite'), conflictSafety = join(directory, 'conflict-safety.sqlite');
  const conflicting = new HiveStore(conflictingPath), objects = new Objects(conflicting);
  objects.bootstrapScopeRoots([rootId], at);
  objects.register(contract('agent/skills'), { kind: 'agent' });
  addObject(conflicting, rootId, 'agent/skills', randomUUID(), 'Skills', { selected: ['other'] });
  conflicting.close();
  const conflict = await planSettingsRestore(backupPath, conflictingPath, backup.sourceHash);
  assert.equal(conflict.conflicts, 1);
  await assert.rejects(restoreSettings(backupPath, conflictingPath, backup.sourceHash, conflictSafety), { code: 'target_conflict' });
});
