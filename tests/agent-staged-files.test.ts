import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, stat, utimes } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { stageFile, stagedName } from '../services/agent-manager/src/staged-files.js';

test('staged message files stay inside their content directory and expire', async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-staged-')); t.after(() => rm(root, { recursive: true, force: true }));
  assert.equal(stagedName('../../etc/passwd'), 'passwd');
  assert.equal(stagedName('C:\\temp\\..'), 'file');
  assert.equal(stagedName('con.txt'), '_con.txt');
  assert.equal(stagedName('a<b>:c?.png'), 'a_b__c_.png');
  const dataBase64 = Buffer.from('screenshot').toString('base64');
  const first = await stageFile(root, { name: '../shot.png', dataBase64 }), again = await stageFile(root, { name: '../shot.png', dataBase64 });
  assert.deepEqual(again, first);
  assert.equal(dirname(dirname(first.path)), root);
  assert.equal(await readFile(first.path, 'utf8'), 'screenshot');
  await assert.rejects(stageFile(root, { name: 'x', dataBase64: 'not base64!' }), { code: 'invalid_arguments' });
  const old = new Date(Date.now() - 31 * 24 * 60 * 60 * 1000); await utimes(dirname(first.path), old, old);
  await stageFile(root, { name: 'other.txt', dataBase64: Buffer.from('other').toString('base64') });
  await assert.rejects(stat(first.path), { code: 'ENOENT' });
});
