import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { DatabaseSync, backup } from 'node:sqlite';

const [mode, databasePath, ...otherPaths] = process.argv.slice(2);
const [major, minor] = process.versions.node.split('.').map(Number);
assert.equal(major, 24);
assert.ok(minor >= 18);
const raw = ' \t{"s":"\\ud800","n":1.00,"e":1e+01,"emoji":"😀"} \n';
function engine(db) {
  return {
    node: process.version,
    ...db.prepare('SELECT sqlite_version() AS version,sqlite_source_id() AS sourceId').get(),
    options: db.prepare('PRAGMA compile_options').all().map(row => row.compile_options).sort(),
  };
}
function snapshot(db, rows) {
  const result = db.prepare('SELECT count(*) AS rows,sum(id) AS ids,sum(length(payload)) AS bytes,sum(length(CAST(raw AS BLOB))) AS textBytes FROM entries').get();
  assert.equal(result.rows, rows);
  assert.equal(result.ids, rows * (rows + 1) / 2);
  assert.equal(result.bytes, 128 * 2048 + (rows === 129 ? 6 : 0));
  assert.equal(result.textBytes, rows * Buffer.byteLength(raw));
  assert.equal(db.prepare('SELECT raw FROM entries WHERE id=1').get().raw, raw);
  assert.equal(db.prepare('SELECT count(*) AS n FROM entries WHERE id IN (777,999)').get().n, 0);
  assert.equal(db.prepare('PRAGMA integrity_check').get().integrity_check, 'ok');
  assert.equal(db.prepare('PRAGMA user_version').get().user_version, 73);
  if (rows === 129) assert.equal(db.prepare('SELECT hex(payload) AS value FROM entries WHERE id=129').get().value, '6E6174697665');
  return { ...result, rawDigest: 'sha256:' + createHash('sha256').update(raw).digest('hex') };
}
if (mode === 'wal-owner') {
  const db = new DatabaseSync(databasePath);
  db.exec('PRAGMA page_size=1024; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA wal_autocheckpoint=0; PRAGMA user_version=73; CREATE TABLE entries(id INTEGER PRIMARY KEY,raw TEXT NOT NULL,payload BLOB NOT NULL) STRICT; BEGIN IMMEDIATE;');
  const insert = db.prepare('INSERT INTO entries VALUES(?,?,?)');
  for (let id = 1; id <= 128; id++) insert.run(id, raw, Buffer.alloc(2048, id));
  db.exec('COMMIT');
  const progress = [];
  const pages = await backup(db, otherPaths[0], { rate: 1, progress: value => progress.push(value) });
  assert.ok(progress.length > 0);
  // Promise completion is authoritative; retain the reported progress exactly.
  assert.ok(progress.at(-1).remainingPages >= 0 && progress.at(-1).remainingPages <= pages);
  assert.equal(progress.at(-1).totalPages, pages);
  const result = { engine: engine(db), snapshot: snapshot(db, 128), backup: { pages, progress } };
  db.exec("BEGIN IMMEDIATE; INSERT INTO entries VALUES(999,'uncommitted',zeroblob(5));");
  process.stdout.write(JSON.stringify(result) + '\n');
  // The parent kills this owned fixture while the write is uncommitted.
  process.stdin.resume();
  process.stdin.on('end', () => { db.close(); process.exit(0); });
} else if (mode === 'verify') {
  const databases = [databasePath, ...otherPaths].map((file, index) => {
    const db = new DatabaseSync(file, { readOnly: true });
    const result = { engine: engine(db), snapshot: snapshot(db, index === 0 ? 129 : 128) };
    db.close();
    return result;
  });
  process.stdout.write(JSON.stringify(databases));
} else {
  throw Error('Unknown SQLite fixture mode.');
}
