import test from 'node:test';
import assert from 'node:assert/strict';
import { DatabaseSync } from 'node:sqlite';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { ReceiptArchive } from '../packages/sdk/src/receipt-archive.js';
import { operationId } from '../packages/contracts/src/operation-id.js';
import { digest } from '../packages/contracts/src/canonical.js';
import type { RpcClient } from '../packages/sdk/src/client.js';
import type { TaskBoard } from '../packages/contracts/src/generated.js';
import { CollectorStore } from '../services/data-collector/src/store.js';
import type { Run } from '../services/data-collector/src/store.js';
import { SecretaryStore } from '../services/secretary/src/store.js';
import { TaskBoardStore } from '../services/task-board/src/runtime/store.js';
import { TaskBoardOperations } from '../services/task-board/src/runtime/operations.js';
import { LocalAgentState } from '../services/agent-manager/src/hive-state.js';

test('released receipts form a FIFO by count and bytes, preserve uncertainty and retain replay guards across restart', t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-history-')), path = join(root, 'receipts.sqlite');
  let db = new DatabaseSync(path);
  t.after(() => { db.close(); rmSync(root, { recursive: true, force: true }); });
  let archive = new ReceiptArchive(db, { count: 2, bytes: 100 });
  const unknown = JSON.stringify({ phase: 'outcome_unknown' });
  archive.retain('command', 'unknown', unknown);
  assert.throws(() => archive.release('command', 'unknown'), { code: 'receipt_archive_unresolved' });
  for (const id of ['first', 'second']) { archive.retain('command', id, id); archive.release('command', id); }
  assert.equal(archive.read('command', 'first'), 'first');
  archive.retain('command', 'third', 'third'); archive.release('command', 'third');
  assert.throws(() => archive.read('command', 'first'), { code: 'operation_expired' });
  assert.equal(archive.read('command', 'unknown'), unknown);
  assert.throws(() => archive.retain('command', 'first', 'changed'), { code: 'receipt_archive_conflict' });
  db.exec('BEGIN IMMEDIATE');
  archive.retain('command', 'fourth', 'fourth'); archive.release('command', 'fourth');
  db.exec('ROLLBACK');
  assert.equal(archive.read('command', 'second'), 'second');
  assert.equal(archive.read('command', 'fourth'), null);
  archive.limits.bytes = 6;
  archive.retain('command', 'large', 'abcdef'); archive.release('command', 'large');
  assert.equal(archive.read('command', 'large'), 'abcdef');
  assert.throws(() => archive.read('command', 'third'), { code: 'operation_expired' });
  assert.equal(archive.status().records, 2);
  assert.equal(archive.status().bytes, Buffer.byteLength(unknown) + 6);
  db.close(); db = new DatabaseSync(path); archive = new ReceiptArchive(db);
  assert.throws(() => archive.read('command', 'first'), { code: 'operation_expired' });
  assert.equal(archive.read('command', 'unknown'), unknown);
});

test('Agent history preserves pending turn ownership and unknown outcomes, then releases only the confirmed turn', async t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-agent-history-')), path = join(root, 'state.sqlite');
  const limits = { maxOperations: 3, maxJournalBytes: 64 * 1024 * 1024 };
  let store = new LocalAgentState(path, 'node', limits);
  t.after(() => { store.close(); rmSync(root, { recursive: true, force: true }); });
  store.setRuntimeEpoch('epoch');
  const at = Date.now();
  const old = { operationId: operationId('epoch', at - 1000, 'old'), phase: 'succeeded' };
  await store.write('operation', 'old', old);
  await store.write('operation', 'unknown', { operationId: operationId('epoch', at - 2000, 'unknown'), phase: 'outcome_unknown' });
  await store.write('operation', 'turn', { operationId: operationId('epoch', at - 500, 'turn'), phase: 'succeeded', method: 'turn/start',
    callerPrincipalId: 'caller', params: { threadId: 'thread' }, reply: { result: { turn: { id: 'turn' } } } });
  await store.write('operation', 'new', { operationId: operationId('epoch', at, 'new'), phase: 'failed' });
  assert.equal(await store.read('operation', 'old'), null);
  assert.ok(await store.read('operation', 'unknown'));
  assert.equal(store.completionCaller('thread', 'turn'), 'caller');
  store.close(); store = new LocalAgentState(path, 'node', limits); store.setRuntimeEpoch('epoch');
  assert.equal(store.completionCaller('thread', 'turn'), 'caller');
  store.releaseCompletion('thread', 'turn');
  await store.write('operation', 'last', { operationId: operationId('epoch', at + 1, 'last'), phase: 'failed' });
  assert.equal(await store.read('operation', 'turn'), null);
  assert.ok(await store.read('operation', 'unknown'));
  await assert.rejects(store.write('operation', 'old', old), { code: 'operation_expired' });
});

test('Agent byte pressure and a failed insert do not lose receipts or advance the replay fence', async t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-agent-byte-ring-')), path = join(root, 'state.sqlite');
  const store = new LocalAgentState(path, 'node', { maxOperations: 100, maxJournalBytes: 1024 * 1024 + 4096 });
  let db: DatabaseSync;
  t.after(() => { db?.close(); store.close(); rmSync(root, { recursive: true, force: true }); });
  store.setRuntimeEpoch('epoch');
  const original = { operationId: operationId('epoch', Date.now() - 1000, 'first'), phase: 'succeeded', payload: 'x'.repeat(1024 * 1024) };
  await store.write('operation', 'first', original);
  db = new DatabaseSync(path);
  db.exec("CREATE TRIGGER reject_insert BEFORE INSERT ON state WHEN NEW.key='reject' BEGIN SELECT RAISE(ABORT,'rejected'); END;");
  await assert.rejects(store.write('operation', 'reject', { ...original, operationId: operationId('epoch') }), /rejected/);
  assert.deepEqual((await store.read<typeof original>('operation', 'first'))?.value, original);
  assert.equal(db.prepare("SELECT value FROM state_metadata WHERE key='expired_before'").get()?.['value'], '0');
  await store.write('operation', 'next', { ...original, operationId: operationId('epoch') });
  assert.equal(await store.read('operation', 'first'), null);
});

test('Agent admission reserves each outstanding result and keeps that reservation across restart', async t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-agent-reservations-')), path = join(root, 'state.sqlite');
  const limits = { maxOperations: 100, maxJournalBytes: 26 * 1024 * 1024 };
  let store = new LocalAgentState(path, 'node', limits);
  t.after(() => { store.close(); rmSync(root, { recursive: true, force: true }); });
  store.setRuntimeEpoch('epoch');
  const first = await store.write('operation', 'first', { operationId: operationId('epoch'), phase: 'accepted' });
  store.close(); store = new LocalAgentState(path, 'node', limits); store.setRuntimeEpoch('epoch');
  const next = { operationId: operationId('epoch'), phase: 'accepted' };
  await assert.rejects(store.write('operation', 'next', next), { code: 'native_journal_capacity' });
  assert.equal(await store.read('operation', 'next'), null);
  await store.write('operation', 'first', { ...first.value, phase: 'outcome_unknown' }, first.pin);
  await store.write('operation', 'next', next);
  assert.ok(await store.read('operation', 'first'));
});

test('Secretary pressure retires only released technical history and rolls eviction back with a failed write', t => {
  const store = new SecretaryStore({} as RpcClient, 'root', undefined, { count: 2, bytes: 2048 }); t.after(() => store.close());
  const id = operationId('epoch', Date.now() - 1000, 'first');
  const first = store.technicalCreate('secretary/public-operation', 'first', { operationId: id, phase: 'succeeded', text: 'x'.repeat(500) });
  store.completeTechnicalWorkflow(first.pin.objectId);
  store.technicalCreate('secretary/follow-up', 'unknown', { request: { operationId: operationId('epoch') }, phase: 'outcome_unknown', expiresAt: 0 });
  store.db.exec("CREATE TRIGGER reject_technical BEFORE INSERT ON technical_documents WHEN NEW.name='reject' BEGIN SELECT RAISE(ABORT,'rejected'); END;");
  assert.throws(() => store.technicalCreate('test/row', 'reject', { phase: 'running' }), /rejected/);
  assert.ok(store.technicalNamed('secretary/public-operation', 'first'));
  assert.equal(store.expiredBefore(), 0);
  store.technicalCreate('test/row', 'next', { phase: 'running' });
  assert.equal(store.technicalNamed('secretary/public-operation', 'first'), null);
  assert.ok(store.technicalNamed('secretary/follow-up', 'unknown'));
  assert.ok(store.expiredBefore() >= Date.now() - 2000);
  store.cleanup(Date.now() + 90 * 86400_000);
  assert.ok(store.technicalNamed('secretary/follow-up', 'unknown'));
});

test('TaskBoard completed payloads leave a durable identity guard while unfinished work survives', async t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-task-history-'));
  const store = new TaskBoardStore({} as RpcClient, null, root, { count: 2, bytes: 65536 });
  t.after(() => { store.close(); rmSync(root, { recursive: true, force: true }); });
  const value = { schemaVersion: 1, operationId: 'opaque-first', callerPrincipalId: 'caller', phase: 'succeeded',
    actor: { source: 'user' }, request: { action: 'edit' }, requestHash: digest('request'), writes: [], nativeCalls: [], outcome: null, error: null } as unknown as TaskBoard.Operation;
  const first = await store.write('task-board/operation', value, 'write', { create: { parentId: null, name: 'first' } });
  store.completeLocalWorkflow(first.objectId);
  const unknown = await store.write('task-board/operation', { ...value, phase: 'needs_attention', operationId: 'unknown' }, 'unknown', { create: { parentId: null, name: 'unknown' } });
  store.completeLocalWorkflow(unknown.objectId);
  await store.write('task-board/operation', { ...value, phase: 'accepted', operationId: 'next' }, 'next', { create: { parentId: null, name: 'next' } });
  assert.equal(store.retiredOperation('caller', 'opaque-first'), value.requestHash);
  assert.ok(await store.read('task-board/operation', unknown));
  // Public names hash caller and ID; test that exact identity independently of its display name.
  const operations = new TaskBoardOperations(store);
  await assert.rejects(operations.find('caller', 'opaque-first'), { code: 'operation_expired' });
});

test('Collector result history protects publications, unknown executions, latest status and prepared definition writes', t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-collector-history-'));
  const store = new CollectorStore(join(root, 'collector.sqlite'), { operations: 2, operationBytes: 65536, runs: 1, runBytes: 65536 });
  t.after(() => { store.close(); rmSync(root, { recursive: true, force: true }); });
  const run = { id: 'old', taskId: 'task', task: { id: 'task', script: 'original', config: {}, dependencies: {} }, status: 'succeeded',
    input: { original: true }, output: null, startedAt: new Date().toISOString(), finishedAt: new Date().toISOString(), eventIndex: 0, error: null } as Run;
  store.saveRun(run);
  store.saveRun({ ...run, id: 'unknown', status: 'interrupted', error: 'outcome_unknown' });
  store.saveRun({ ...run, id: 'pending', status: 'publishing', output: { data: { retained: true } } });
  store.setMetadata('definition-expiry:prepared', '0');
  store.setMetadata('definition-write:prepared:step', 'original prepared bytes');
  store.cleanup();
  assert.equal(store.run('old'), null);
  assert.equal(store.usedRunId('old'), true);
  assert.equal(store.run('unknown')?.task.script, 'original');
  assert.deepEqual(store.run('pending')?.output, { data: { retained: true } });
  assert.equal(store.metadata('definition-write:prepared:step'), 'original prepared bytes');
  store.mutate('old-operation', 'original', () => ({ saved: true }));
  store.mutate('pending-operation', 'pending', () => ({ runId: 'pending' }));
  store.mutate('next-operation', 'next', () => ({ saved: true }));
  let calls = 0;
  assert.throws(() => store.mutate('old-operation', 'original', () => calls++), { code: 'operation_expired' });
  assert.throws(() => store.mutate('old-operation', 'changed', () => calls++), { code: 'operation_conflict' });
  assert.equal(calls, 0);
  assert.deepEqual(store.operation('pending-operation'), { runId: 'pending' });
  assert.throws(() => store.db.prepare('INSERT INTO operations VALUES(?,?,?,?)').run('old-operation', 'original', '{}', Date.now()), /operation_expired/);
});

test('Collector backlog collection computes aggregate usage once per ring', t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-collector-backlog-'));
  const store = new CollectorStore(join(root, 'collector.sqlite'), { operations: 2, operationBytes: 65536, runs: 2, runBytes: 65536 });
  t.after(() => { store.close(); rmSync(root, { recursive: true, force: true }); });
  store.db.exec('BEGIN IMMEDIATE');
  for (let i = 0; i < 1000; i++) {
    const id = 'old-' + i;
    store.db.prepare('INSERT INTO operations VALUES(?,?,?,?)').run(id, '{}', '{}', i);
    const run = { id, taskId: 'task', status: 'succeeded', error: null, finishedAt: new Date(i).toISOString() };
    store.db.prepare('INSERT INTO runs VALUES(?,?,?,?)').run(id, 'task', 'succeeded', JSON.stringify(run));
  }
  const unknown = { id: 'unknown', taskId: 'task', status: 'failed', error: 'outcome_unknown' };
  store.db.prepare('INSERT INTO runs VALUES(?,?,?,?)').run('unknown', 'task', 'failed', JSON.stringify(unknown));
  store.db.exec('COMMIT');
  let aggregates = 0;
  const prepare = store.db.prepare.bind(store.db);
  t.mock.method(store.db, 'prepare', (sql: string) => {
    if (sql.includes('SUM(')) aggregates++;
    return prepare(sql);
  });
  store.cleanup();
  assert.equal(aggregates, 2, 'Collection must not rescan the remaining payloads after each eviction.');
  assert.equal(store.db.prepare('SELECT COUNT(*) AS count FROM operations').get()!['count'], 2);
  assert.equal(store.db.prepare('SELECT COUNT(*) AS count FROM runs').get()!['count'], 2);
  assert.equal(store.run('unknown')?.error, 'outcome_unknown');
});

test('Collector completion evicts closed run history without waiting for maintenance', t => {
  const root = mkdtempSync(join(tmpdir(), 'ivy-collector-completion-ring-'));
  const store = new CollectorStore(join(root, 'collector.sqlite'), { operations: 10, operationBytes: 65536, runs: 2, runBytes: 65536 });
  t.after(() => { store.close(); rmSync(root, { recursive: true, force: true }); });
  const run = { id: 'unknown', taskId: 'task', status: 'failed', error: 'outcome_unknown', task: { script: '', config: {}, dependencies: {} } } as Run;
  store.saveRun(run);
  store.saveRun({ ...run, id: 'first', status: 'succeeded', error: null, finishedAt: new Date().toISOString() });
  store.transaction(() => store.saveRun({ ...run, id: 'second', status: 'succeeded', error: null, finishedAt: new Date().toISOString() }));
  assert.equal(store.run('first'), null);
  assert.equal(store.run('unknown')?.error, 'outcome_unknown');
  assert.equal(store.run('second')?.status, 'succeeded');
});
