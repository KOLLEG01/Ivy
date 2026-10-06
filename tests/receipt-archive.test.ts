import test from 'node:test';
import assert from 'node:assert/strict';
import { DatabaseSync } from 'node:sqlite';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import { ReceiptArchive } from '../packages/sdk/src/receipt-archive.js';

test('archive copy and hot deletion roll back together; retained original receipts are immutable', t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-receipt-archive-')), path = join(root, 'archive.sqlite');
  let db = new DatabaseSync(path);
  t.after(() => { db.close(); rmSync(root, { recursive: true, force: true }); });
  db.exec('PRAGMA synchronous=FULL; CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE hot(id TEXT PRIMARY KEY,value TEXT); INSERT INTO hot VALUES (\'original\',\'receipt\');');
  const archive = new ReceiptArchive(db);
  db.exec('BEGIN IMMEDIATE'); archive.retain('action', 'original', 'receipt'); db.exec('DELETE FROM hot; ROLLBACK');
  assert.equal(archive.read('action', 'original'), null); assert.ok(db.prepare('SELECT 1 FROM hot').get());
  db.exec('BEGIN IMMEDIATE'); archive.retain('action', 'original', 'receipt'); db.exec('DELETE FROM hot; COMMIT');
  assert.equal(archive.read('action', 'original'), 'receipt'); assert.equal(archive.status().records, 1);
  assert.throws(() => archive.retain('action', 'original', 'changed'), { code: 'receipt_archive_conflict' });
  assert.deepEqual(archive.status(), { records: 1, bytes: 7 });
  db.close(); db = new DatabaseSync(path);
  const reopened = new ReceiptArchive(db);
  assert.equal(reopened.read('action', 'original'), 'receipt');
  assert.deepEqual(reopened.status(), { records: 1, bytes: 7 });
});
