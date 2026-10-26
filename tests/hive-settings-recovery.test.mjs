import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { mkdtemp, rm, symlink, stat } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { DatabaseSync } from 'node:sqlite';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { randomUUID } from 'node:crypto';
import { canonical, digest } from '../dist/packages/contracts/src/canonical.js';
import { artifactRoot, nativeExecutable } from '../tools/testing/native-service.mjs';

const at = '2026-09-23T00:00:00.000Z';
const executable = nativeExecutable(artifactRoot('hive'), 'ivy-hive');
const ddl = readFileSync(new URL('fixtures/hive-format-four.sql', import.meta.url), 'utf8');
const contract = key => ({ key, version: '1.0.0', owner: { kind: 'agent' }, mediaType: 'application/json',
  retention: { objects: { mode: 'retain' }, revisions: { mode: 'current' } },
  jsonSchema: { type: 'object', additionalProperties: true }, specMarkdown: 'Settings recovery fixture.' });
function register(db, key, definition = contract(key)) {
  db.prepare('INSERT OR IGNORE INTO contract_families VALUES (?,?,?,?)').run(key, '{"kind":"agent"}', 'application/json', 'retain');
  db.prepare('INSERT INTO contracts VALUES (?,?,?)').run(key, '1.0.0', canonical(definition));
}
function addObject(db, rootId, key, id, name, value) {
  const parent = rootId && db.prepare('SELECT path FROM objects WHERE id=?').get(rootId), content = canonical(value);
  db.exec('BEGIN');
  try {
    db.prepare('INSERT INTO objects(id,parent_id,owner_object_id,name,path,position,icon,depth,contract_key,current_revision,archived_at,effective_archive,created_at,updated_at) VALUES (?,?,NULL,?,?,0,NULL,?, ?,1,NULL,0,?,?)')
      .run(id, rootId, name, parent ? parent.path + '/' + name : name, rootId ? 2 : 1, key, at, at);
    db.prepare("INSERT INTO revisions(object_id,revision,contract_key,contract_version,encoding,content,content_hash,byte_length,created_at,references_json) VALUES (?,1,?,'1.0.0','json',?,?,?,?,'{}')")
      .run(id, key, content, digest(content), Buffer.byteLength(content), at);
    db.prepare('INSERT INTO object_fts(object_id,name,text) VALUES (?,?,?)').run(id, name, '');
    db.exec('COMMIT');
  } catch (error) { db.exec('ROLLBACK'); throw error; }
}
function seed(path, rootId) {
  const db = new DatabaseSync(path);
  db.exec(ddl + '; PRAGMA user_version=4');
  register(db, 'fixture/root');
  addObject(db, null, 'fixture/root', rootId, 'Scope', {});
  return db;
}
function invoke(args, selected = executable) {
  const result = spawnSync(selected, ['settings-recovery', ...args], { encoding: 'utf8', windowsHide: true, timeout: 15000 });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const error = new Error(result.stderr); error.code = JSON.parse(result.stderr).code; throw error;
  }
  assert.equal(result.stderr, '');
  const output = JSON.parse(result.stdout); assert.equal(output.ok, true); return output.result;
}

test('offline settings backup restores only missing allowlisted Objects and preserves current data', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'ivy-settings-recovery-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const sourcePath = join(directory, 'source.sqlite'), targetPath = join(directory, 'target.sqlite'),
    backupPath = join(directory, 'backup.sqlite'), safetyPath = join(directory, 'safety.sqlite');
  const rootId = randomUUID(), skillsId = randomUUID(), wikiId = randomUUID();
  const source = seed(sourcePath, rootId);
  for (const key of ['agent/skills', 'wiki/page']) register(source, key);
  addObject(source, rootId, 'agent/skills', skillsId, 'Skills', { schemaVersion: 1, selected: ['alpha'] });
  addObject(source, rootId, 'wiki/page', wikiId, 'Private page', { body: 'not a setting' });
  source.close(); seed(targetPath, rootId).close();
  const before = readFileSync(sourcePath);
  const backup = invoke(['backup', sourcePath, backupPath]);
  assert.equal(backup.total, 1); assert.match(backup.sourceHash, /^sha256:[a-f0-9]{64}$/);
  if (process.platform !== 'win32') assert.equal((await stat(backupPath)).mode & 0o777, 0o600);
  const args = ['plan', backupPath, targetPath, backup.sourceHash];
  assert.equal(invoke(args).restore, 1);
  const linkedPath = join(directory, process.platform === 'win32' ? 'settings-recovery-link.exe' : 'settings-recovery-link');
  try {
    await symlink(executable, linkedPath);
    assert.equal(invoke(args, linkedPath).restore, 1);
  } catch (error) { if (error.code !== 'EPERM') throw error; }
  const preview = invoke(args);
  assert.equal(preview.restore, 1); assert.equal(preview.conflicts, 0);
  const restored = invoke(['restore', backupPath, targetPath, backup.sourceHash, safetyPath]);
  assert.equal(restored.restore, 1); assert.match(restored.safetyHash, /^sha256:[a-f0-9]{64}$/);
  const check = new DatabaseSync(targetPath);
  assert.equal(check.prepare('SELECT contract_key FROM objects WHERE id=?').get(skillsId).contract_key, 'agent/skills');
  assert.equal(check.prepare('SELECT id FROM objects WHERE id=?').get(wikiId), undefined);
  assert.equal(check.prepare('PRAGMA foreign_key_check').get(), undefined);
  check.close();
  const repeated = invoke(args); assert.equal(repeated.restore, 0); assert.equal(repeated.preserve, 1);
  assert.throws(() => invoke(['plan', backupPath, targetPath, 'sha256:' + '0'.repeat(64)]), { code: 'backup_hash_mismatch' });
  assert.deepEqual(readFileSync(sourcePath), before);
  const conflictingPath = join(directory, 'conflicting.sqlite'), conflictSafety = join(directory, 'conflict-safety.sqlite');
  const conflicting = seed(conflictingPath, rootId); register(conflicting, 'agent/skills');
  addObject(conflicting, rootId, 'agent/skills', randomUUID(), 'Skills', { selected: ['other'] }); conflicting.close();
  assert.equal(invoke(['plan', backupPath, conflictingPath, backup.sourceHash]).conflicts, 1);
  const conflictBefore = readFileSync(conflictingPath);
  assert.throws(() => invoke(['restore', backupPath, conflictingPath, backup.sourceHash, conflictSafety]), { code: 'target_conflict' });
  assert.deepEqual(readFileSync(conflictingPath), conflictBefore);
});

test('settings recovery preserves differing current Objects and rejects immutable contracts and references', async t => {
  const directory = await mkdtemp(join(tmpdir(), 'ivy-settings-recovery-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const rootId = randomUUID(), id = randomUUID(), sourcePath = join(directory, 'source.sqlite'), backupPath = join(directory, 'backup.sqlite');
  const source = seed(sourcePath, rootId); register(source, 'agent/skills');
  addObject(source, rootId, 'agent/skills', id, 'Skills', { selected: ['alpha'] }); source.close();
  const backup = invoke(['backup', sourcePath, backupPath]);
  for (const mode of ['different', 'contract', 'parent', 'identity']) {
    const path = join(directory, mode + '.sqlite'), db = seed(path, mode === 'parent' ? randomUUID() : rootId);
    if (mode === 'different' || mode === 'identity') {
      register(db, 'agent/skills'); addObject(db, rootId, 'agent/skills', id, mode === 'identity' ? 'Changed name' : 'Skills', { selected: ['current'] });
    } else if (mode === 'contract') register(db, 'agent/skills', { ...contract('agent/skills'), specMarkdown: 'Conflicting immutable version.' });
    db.close(); const before = readFileSync(path);
    const preview = invoke(['plan', backupPath, path, backup.sourceHash]);
    if (mode === 'different') {
      assert.equal(preview.preserve, 1); assert.equal(preview.preserveDifferent, 1); assert.equal(preview.conflicts, 0);
      invoke(['restore', backupPath, path, backup.sourceHash, join(directory, mode + '-safety.sqlite')]);
      const db = new DatabaseSync(path, { readOnly: true });
      assert.equal(JSON.parse(db.prepare('SELECT content FROM revisions WHERE object_id=?').get(id).content).selected[0], 'current'); db.close();
    } else {
      assert.equal(preview.conflicts, 1);
      assert.throws(() => invoke(['restore', backupPath, path, backup.sourceHash, join(directory, mode + '-safety.sqlite')]), { code: 'target_conflict' });
      assert.deepEqual(readFileSync(path), before);
    }
  }
  const referenced = join(directory, 'referenced.sqlite'), db = seed(referenced, rootId);
  register(db, 'agent/skills'); addObject(db, rootId, 'agent/skills', id, 'Skills', {});
  db.exec('DROP TRIGGER revisions_immutable_update');
  db.prepare('UPDATE revisions SET references_json=? WHERE object_id=?').run('{"linked":{"objectId":"missing","revision":1}}', id); db.close();
  const refs = invoke(['backup', referenced, join(directory, 'refs-backup.sqlite')]);
  const target = join(directory, 'empty.sqlite'); seed(target, rootId).close();
  assert.equal(invoke(['plan', join(directory, 'refs-backup.sqlite'), target, refs.sourceHash]).conflicts, 1);
});