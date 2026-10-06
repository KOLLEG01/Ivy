import assert from 'node:assert/strict';
import { DatabaseSync } from 'node:sqlite';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { createHash } from 'node:crypto';
import { parseArgs } from 'node:util';

// Bounded comparison of FTS identity lookup strategies, not a substitute for
// the complete Hive workload or a product implementation of a different index.
const { values } = parseArgs({ options: { output: { type: 'string' } } });
assert.ok(values.output);
const root = resolve(values.output); assert.ok(root.split(/[\\/]/).includes('.local'));
await mkdir(root);
const schemaBytes = await readFile(new URL('../../../services/hive/src/storage-schema.ts', import.meta.url));
const schema = schemaBytes.toString('utf8').match(/CREATE VIRTUAL TABLE object_fts USING fts5\([^;]+;/)?.[0];
assert.ok(schema && schema.includes('object_id UNINDEXED'));
const report = { schemaVersion: 1, phase: 'running', startedAt: new Date().toISOString(),
  schemaHash: 'sha256:' + createHash('sha256').update(schemaBytes).digest('hex'), schema,
  budget: { rowCounts: [100, 1000, 5000], contentBytes: 10240, measuredUpdatesPerStrategy: 20, maximumElapsedMs: 180000, modelTurns: 0 },
  scope: 'In-memory FTS DELETE identity lookup followed by identical reinsertion. Excludes disk durability, Hive protocol and other write costs.', cases: [] };
const path = join(root, 'report.json'), started = performance.now();
const save = () => writeFile(path, JSON.stringify(report, null, 2) + '\n', { mode: 0o600 });
const withinBudget = () => assert.ok(performance.now() - started <= report.budget.maximumElapsedMs, 'FTS profile exceeded its original bounded budget');
await save();
try {
  for (const rows of report.budget.rowCounts) {
    const db = new DatabaseSync(':memory:');
    try {
      db.exec(schema);
      const insert = db.prepare('INSERT INTO object_fts(rowid,object_id,name,text) VALUES (?,?,?,?)');
      const payload = 'searching '.repeat(1024);
      assert.equal(Buffer.byteLength(payload), report.budget.contentBytes);
      db.exec('BEGIN');
      for (let index = 1; index <= rows; index++) { withinBudget(); insert.run(index, 'object-' + index, 'Name ' + index, payload); }
      db.exec('COMMIT');
      const target = Math.ceil(rows / 2), id = 'object-' + target;
      const sample = { rows, bytes: rows * report.budget.contentBytes, strategies: [] };
      for (const [name, sql, parameter] of [
        ['unindexed-object-id', 'DELETE FROM object_fts WHERE object_id=?', id],
        ['integer-rowid', 'DELETE FROM object_fts WHERE rowid=?', target],
      ]) {
        const statement = db.prepare(sql), times = [];
        const queryPlan = db.prepare('EXPLAIN QUERY PLAN ' + sql).all(parameter);
        for (let index = 0; index < report.budget.measuredUpdatesPerStrategy; index++) {
          withinBudget(); db.exec('BEGIN');
          const tick = performance.now();
          assert.equal(statement.run(parameter).changes, 1);
          insert.run(target, id, 'Name ' + target, payload);
          db.exec('COMMIT'); times.push(performance.now() - tick);
        }
        assert.equal(db.prepare('SELECT count(*) AS n FROM object_fts').get().n, rows);
        assert.equal(db.prepare('SELECT object_id FROM object_fts WHERE rowid=?').get(target).object_id, id);
        assert.equal(db.prepare("SELECT count(*) AS n FROM object_fts WHERE object_fts MATCH 'searching'").get().n, rows);
        times.sort((a, b) => a - b);
        sample.strategies.push({ name, queryPlan, count: times.length, medianMs: times[Math.ceil(times.length / 2) - 1],
          p95Ms: times[Math.ceil(times.length * 0.95) - 1], elapsedMs: times.reduce((sum, value) => sum + value, 0), timesMs: times });
      }
      report.cases.push(sample); await save();
    } finally { db.close(); }
  }
  report.phase = 'passed';
} catch (error) { report.phase = 'failed'; report.error = { code: error.code ?? error.name, message: error.message }; process.exitCode = 1; }
finally { report.elapsedMs = performance.now() - started; report.memory = process.memoryUsage(); report.finishedAt = new Date().toISOString(); await save(); }
console.log(JSON.stringify({ phase: report.phase, report: path, elapsedMs: report.elapsedMs }));
