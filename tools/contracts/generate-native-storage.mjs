import { readFileSync, writeFileSync, readdirSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { gunzipSync } from 'node:zlib';
import { DatabaseSync } from 'node:sqlite';
import assert from 'node:assert/strict';
import { nativeVersions } from './checked-native-versions.mjs';

// These inputs contain only reviewed tagged upstream SQL and explicit relocation columns.
// No native process, network request or user's database participates in generation.
for (const version of nativeVersions) {
  const root = 'specs/native/codex-' + version;
  for (const file of readdirSync(root).filter(name => /^storage-source(?:\.(?:linux|win32|darwin)-(?:x64|arm64))?\.json\.gz$/.test(name))) {
  const input = JSON.parse(gunzipSync(readFileSync(root + '/' + file), { maxOutputLength: 1024 * 1024 }));
  assert.equal(input.schemaVersion, 1); assert.equal(input.nativeVersion, version);
  assert.equal(input.database, 'state_5.sqlite');
  assert.equal(input.upstream.tag, 'rust-v' + version);
  assert.match(input.upstream.tree, /^[a-f0-9]{40}$/);
  assert.ok(['lf', 'crlf'].includes(input.upstream.migrationLineEndings));
  assert.ok(input.files.length > 0 && input.files.length <= 1000);
  const db = new DatabaseSync(':memory:'), migrations = [];
  try {
    let previous = 0;
    for (const file of input.files) {
      assert.match(file.name, /^\d{4}_[a-z0-9_]+\.sql$/);
      const id = Number(file.name.slice(0, 4)); assert.ok(id > previous); previous = id;
      const source = Buffer.from(file.sql);
      assert.equal(createHash('sha1').update('blob ' + source.length + '\0').update(source).digest('hex'), file.blob);
      const sql = file.sql.replaceAll('\r\n', '\n');
      const bytes = Buffer.from(input.upstream.migrationLineEndings === 'crlf' ? sql.replaceAll('\n', '\r\n') : sql);
      db.exec(bytes.toString());
      migrations.push({ version: id, description: file.name.slice(5, -4).replaceAll('_', ' '), checksum: createHash('sha384').update(bytes).digest('hex') });
    }
    const schema = db.prepare('SELECT type,name,tbl_name AS tableName,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY type,name')
      .all().map(row => ({ ...row, sql: row.sql.replaceAll('\r\n', '\n') }));
    for (const column of input.pathColumns) {
      assert.ok(['required-rollout', 'rollout-marker', 'optional-owned-file'].includes(column.scope));
      const table = schema.find(entry => entry.type === 'table' && entry.name === column.table); assert.ok(table);
      const columns = db.prepare('SELECT name FROM pragma_table_info(?)').all(column.table);
      assert.ok(columns.some(entry => entry.name === column.column));
    }
    const value = { schemaVersion: 1, nativeVersion: version, database: input.database, upstream: input.upstream, migrations, schema, pathColumns: input.pathColumns };
    const content = JSON.stringify(value, null, 2) + '\n', path = root + '/' + file.replace(/^storage-source/, 'storage').replace(/\.gz$/, '');
    if (process.argv.includes('--check')) assert.equal(readFileSync(path, 'utf8'), content, 'Stale native storage pin: ' + version);
    else writeFileSync(path, content);
    if (!process.argv.includes('--check')) console.log(JSON.stringify({ version, migrations: migrations.length, schemaEntries: schema.length }));
  } finally { db.close(); }
  }
}
