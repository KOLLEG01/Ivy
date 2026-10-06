import test, { type TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { DatabaseSync } from 'node:sqlite';
import { mkdtemp, mkdir, readFile, writeFile, rm, realpath, unlink } from 'node:fs/promises';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { tmpdir } from 'node:os';
import { existsSync } from 'node:fs';
import { relocateNativeStorage } from '../packages/host-runtime/src/native-restore.js';
import { copyVerified, exists, inventoryFiles } from '../packages/host-runtime/src/backup-files.js';
import { jsonFile, inside } from '../packages/host-runtime/src/config.js';
import { fileHash } from '../packages/host-runtime/src/artifact.js';
import { hashJson } from '../packages/contracts/src/canonical.js';
import { IvyError } from '../packages/contracts/src/errors.js';
import type { Host } from '../packages/contracts/src/generated.js';

const code = (value: string) => (error: unknown) => error instanceof IvyError && error.code === value;
const distribution = resolve('.');
async function fixture(t: TestContext, version = '0.154.0') {
  const root = await mkdtemp(join(tmpdir(), 'ivy-native-recovery-'));
  t.after(async () => { const actual = await realpath(root); assert.ok(inside(await realpath(tmpdir()), actual) && relative(await realpath(tmpdir()), actual).startsWith('ivy-native-recovery-')); await rm(actual, { recursive: true, force: true }); });
  const platformLayout = join(distribution, 'specs/native/codex-' + version, 'storage.' + process.platform + '-' + process.arch + '.json');
  const layout = await jsonFile<Host.NativeStorageLayout>(existsSync(platformLayout) ? platformLayout : join(distribution, 'specs/native/codex-' + version + '/storage.json'));
  const oldHome = join(root, 'source/instances/agent/data/native-home'), newHome = join(root, 'target/instances/agent/data/native-home');
  const original: Host.HostConfig = { schemaVersion: 1, hostId: 'native-recovery', runtimeRoot: join(root, 'source'), artifactRoot: join(root, 'artifacts'), stagingRoot: join(root, 'staging'), publicBaseUrl: 'http://127.0.0.1:38901',
    executables: { node: process.execPath }, instances: [{ instanceId: 'agent', serviceNodeId: 'native-recovery.agent', componentId: 'agent-manager', enabled: false, engine: 'process',
      settings: { appServer: { mode: 'owned-stdio' }, nativeHome: oldHome, nativeVersion: version, nativeExecutableHash: hashJson('fixture') } }] };
  const target: Host.HostConfig = { ...original, runtimeRoot: join(root, 'target'), publicBaseUrl: 'http://127.0.0.1:38902',
    restoredFrom: { restoreId: 'fresh-copy', backupId: 'fixture-backup', manifestHash: hashJson('fixture-manifest') },
    instances: original.instances.map(instance => ({ ...instance, settings: { ...instance.settings, nativeHome: newHome } })) };
  const contents = new Map([
    ['sessions/legacy.jsonl', '{"id":"legacy","cwd":' + JSON.stringify(oldHome) + '}\n'],
    ['archived_sessions/archived.jsonl', '{"id":"archived","untouched":true}\n'],
    ['sessions/paginated-selected.jsonl', '{"id":"paginated","parent_rollout_id":"immutable-parent"}\n'],
    ['sessions/paginated-unselected.jsonl', '{"id":"paginated","do_not_select":true}\n'],
    ['goals_1.sqlite', 'Opaque other native database; no path rewriting.'],
    ['config.toml', 'historical_value = ' + JSON.stringify(oldHome)],
    ['jobs/input.csv', 'id,value\n1,original\n'],
  ]);
  for (const [name, text] of contents) { await mkdir(dirname(join(oldHome, name)), { recursive: true }); await writeFile(join(oldHome, name), text); }
  const db = new DatabaseSync(join(oldHome, 'state_5.sqlite'));
  db.exec('PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA wal_autocheckpoint=0');
  // Earlier official log migrations leave this internal table after dropping their AUTOINCREMENT table.
  db.exec('CREATE TABLE retired_logs (id INTEGER PRIMARY KEY AUTOINCREMENT); DROP TABLE retired_logs');
  for (const entry of layout.schema.filter(value => value.type === 'table' && value.name !== 'sqlite_sequence')) db.exec(entry.sql);
  for (const entry of layout.schema.filter(value => value.type !== 'table')) db.exec(entry.sql);
  db.exec('CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TEXT NOT NULL, success BOOLEAN NOT NULL, checksum BLOB NOT NULL, execution_time BIGINT NOT NULL)');
  for (const migration of layout.migrations) db.prepare('INSERT INTO _sqlx_migrations VALUES (?,?,?,?,?,?)').run(migration.version, migration.description, 'fixed-install-time', 1, Buffer.from(migration.checksum, 'hex'), 9007199254740993n);
  for (const [id, file, archived, mode] of [['legacy', 'sessions/legacy.jsonl', 0, 'legacy'], ['archived', 'archived_sessions/archived.jsonl', 1, 'legacy'], ['paginated', 'sessions/paginated-selected.jsonl', 0, 'paginated']] as const) {
    db.prepare('INSERT INTO threads (id,rollout_path,created_at,updated_at,source,model_provider,cwd,title,sandbox_policy,approval_mode,archived,history_mode,tokens_used,agent_path) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)')
      .run(id, join(oldHome, file), 100, 200, 'cli', 'openai', oldHome, oldHome + ' stays literal in title', '{}', 'never', archived, mode, 9007199254740993n, '/root/child');
  }
  db.prepare('INSERT INTO thread_spawn_edges VALUES (?,?,?)').run('legacy', 'paginated', 'completed');
  if (layout.schema.some(entry => entry.name === 'projects')) {
    db.exec("INSERT INTO projects VALUES ('project','Retained project','{}',1,100,200); INSERT INTO thread_sections VALUES ('section','Retained section','blue')");
    db.prepare('INSERT INTO project_roots VALUES (?,?,?)').run('project', 0, oldHome);
    db.exec("UPDATE threads SET name='Named thread', is_pinned=1, project_id='project', thread_section_id='section', section_position=7, section_entered_at_ms=300 WHERE id='paginated'");
    db.prepare('INSERT INTO rollout_migration_skipped_rollouts VALUES (?,?,?,?,?,?)').run('migration', join(oldHome, 'archived_sessions/no-longer-present.jsonl'), 50, 9007199254740993n, 'retained skip reason', 400);
    const attachmentTable = layout.schema.some(entry => entry.name === 'thread_attachments') ? 'thread_attachments' : 'thread_artifacts';
    if (layout.schema.some(entry => entry.name === attachmentTable)) db.prepare('INSERT INTO ' + attachmentTable + ' VALUES (?,?,?,?,?,?)')
      .run('artifact', 'paginated', 'fixture', 'original-identity', JSON.stringify({ originalPath: join(oldHome, 'sessions/paginated-selected.jsonl') }), 9007199254740993n);
  } else for (const [id, input, output] of [['owned', join(oldHome, 'jobs/input.csv'), join(oldHome, 'jobs/not-created.csv')], ['external', join(root, 'project/input.csv'), join(root, 'project/output.csv')]]) {
    db.prepare('INSERT INTO agent_jobs (id,name,status,instruction,input_headers_json,input_csv_path,output_csv_path,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?)')
      .run(id!, 'Saved job', 'completed', oldHome + ' literal instruction', '[]', input!, output!, 100, 200);
  }
  // A controlled fixture snapshot includes committed WAL data while the sole connection is idle.
  // The production backup separately requires actual stopped native/OS owners.
  const captured = await inventoryFiles(original.runtimeRoot), backupRoot = join(root, 'backup-state');
  for (const file of captured) { await copyVerified(join(original.runtimeRoot, file.path), join(backupRoot, file.path), file, true); await copyVerified(join(backupRoot, file.path), join(target.runtimeRoot, file.path), file, true); }
  db.close();
  const manifest: Host.BackupManifest = { schemaVersion: 1, backupId: 'fixture-backup', hostId: original.hostId, createdAt: 'now', completedAt: 'now', consistency: 'offline-owned-state',
    platform: { os: process.platform as 'win32' | 'linux', arch: process.arch, node: process.version }, configurationHash: hashJson(original),
    files: captured.map(file => ({ ...file, path: 'state/' + file.path.split(sep).join('/') })), externalFiles: [], hiveSnapshots: [], excluded: [] };
  return { root, layout, original, target, oldHome, newHome, manifest, backupRoot, captured, contents };
}

for (const version of ['0.154.0', '0.158.0', '0.159.2']) test('native ' + version + ' relocates selected/archived pointers without losing WAL, lineage or opaque state', async t => {
  const f = await fixture(t, version), sourceBefore = await inventoryFiles(f.original.runtimeRoot);
  const report = await relocateNativeStorage(f.original, f.target, f.manifest, distribution);
  assert.equal(report.instances.length, 1); assert.equal(report.instances[0]?.changes.find(value => value.table === 'threads')?.rows, 3);
  assert.ok(report.instances[0]!.originalFiles.some(file => file.path.endsWith('-wal') && file.bytes > 0));
  for (const file of report.instances[0]!.originalFiles) assert.equal(await fileHash(join(f.target.runtimeRoot, file.path)), file.hash);
  const db = new DatabaseSync(join(f.newHome, 'state_5.sqlite'), { readOnly: true });
  try {
    const query = db.prepare('SELECT * FROM threads ORDER BY id'); query.setReadBigInts(true); const rows = query.all();
    assert.deepEqual(rows.map(row => [row['id'], row['rollout_path'], row['archived'], row['history_mode']]), [
      ['archived', join(f.newHome, 'archived_sessions/archived.jsonl'), 1n, 'legacy'], ['legacy', join(f.newHome, 'sessions/legacy.jsonl'), 0n, 'legacy'],
      ['paginated', join(f.newHome, 'sessions/paginated-selected.jsonl'), 0n, 'paginated']]);
    assert.ok(rows.every(row => row['tokens_used'] === 9007199254740993n && row['cwd'] === f.oldHome && row['agent_path'] === '/root/child' && row['title'] === f.oldHome + ' stays literal in title'));
    assert.deepEqual({ ...db.prepare('SELECT * FROM thread_spawn_edges').get() }, { parent_thread_id: 'legacy', child_thread_id: 'paginated', status: 'completed' });
    if (f.layout.schema.some(entry => entry.name === 'projects')) {
      const selected = rows.find(row => row['id'] === 'paginated')!;
      assert.equal(selected['project_id'], 'project'); assert.equal(selected['thread_section_id'], 'section'); assert.equal(selected['section_position'], 7n); assert.equal(selected['is_pinned'], 1n);
      assert.equal(db.prepare('SELECT path FROM project_roots').get()?.['path'], f.oldHome);
      assert.equal(db.prepare('SELECT rollout_path FROM rollout_migration_skipped_rollouts').get()?.['rollout_path'], join(f.newHome, 'archived_sessions/no-longer-present.jsonl'));
      const attachmentTable = f.layout.schema.some(entry => entry.name === 'thread_attachments') ? 'thread_attachments' : 'thread_artifacts';
      if (f.layout.schema.some(entry => entry.name === attachmentTable)) {
        const query = db.prepare('SELECT * FROM ' + attachmentTable); query.setReadBigInts(true);
        assert.deepEqual({ ...query.get() }, { id: 'artifact', thread_id: 'paginated', [attachmentTable === 'thread_attachments' ? 'attachment_type' : 'artifact_type']: 'fixture', identity_key: 'original-identity',
          payload: JSON.stringify({ originalPath: join(f.oldHome, 'sessions/paginated-selected.jsonl') }), created_at: 9007199254740993n });
      }
    } else {
      assert.equal(db.prepare("SELECT input_csv_path FROM agent_jobs WHERE id='owned'").get()?.['input_csv_path'], join(f.newHome, 'jobs/input.csv'));
      assert.equal(db.prepare("SELECT output_csv_path FROM agent_jobs WHERE id='owned'").get()?.['output_csv_path'], join(f.newHome, 'jobs/not-created.csv'));
      assert.equal(db.prepare("SELECT input_csv_path FROM agent_jobs WHERE id='external'").get()?.['input_csv_path'], join(f.root, 'project/input.csv'));
    }
  } finally { db.close(); }
  for (const [path, expected] of f.contents) assert.equal(await readFile(join(f.newHome, path), 'utf8'), expected);
  assert.deepEqual(await inventoryFiles(f.original.runtimeRoot), sourceBefore);
  assert.deepEqual(await inventoryFiles(f.backupRoot), f.captured);
});

for (const mutation of ['checksum', 'trigger', 'missing', 'outside', 'alternate', 'version', 'unknown-version', 'path-version', 'changed-file'] as const) test('native recovery refuses ' + mutation + ' and never publishes success', async t => {
  const f = await fixture(t), database = join(f.newHome, 'state_5.sqlite');
  if (['checksum', 'trigger', 'missing', 'outside'].includes(mutation)) {
    const db = new DatabaseSync(database);
    try {
      if (mutation === 'checksum') db.exec("UPDATE _sqlx_migrations SET checksum=X'00' WHERE version=1");
      if (mutation === 'trigger') db.exec("CREATE TRIGGER malicious_change AFTER UPDATE OF rollout_path ON threads BEGIN UPDATE threads SET title='changed'; END");
      if (mutation === 'missing') db.prepare("UPDATE threads SET rollout_path=? WHERE id='paginated'").run(join(f.oldHome, 'sessions/missing-selected.jsonl'));
      if (mutation === 'outside') db.prepare("UPDATE threads SET rollout_path=? WHERE id='paginated'").run(join(f.root, 'outside.jsonl'));
    } finally { db.close(); }
    // This malformed snapshot is internally file-hash-consistent; format/path checks must catch it.
    for (const suffix of ['', '-wal', '-shm']) {
      const path = database + suffix, key = 'state/instances/agent/data/native-home/state_5.sqlite' + suffix;
      f.manifest.files = f.manifest.files.filter(file => file.path !== key);
      if (await exists(path)) { const entries = await inventoryFiles(f.target.runtimeRoot); const file = entries.find(file => file.path === key.slice(6))!; f.manifest.files.push({ ...file, path: key }); }
    }
  }
  if (mutation === 'alternate') f.manifest.files.push({ path: 'state/instances/agent/data/native-home/state_6.sqlite', bytes: 4, mode: 384, hash: hashJson('future') });
  if (mutation === 'version') f.target.instances[0]!.settings['nativeVersion'] = '0.150.0';
  if (mutation === 'unknown-version' || mutation === 'path-version') {
    const version = mutation === 'unknown-version' ? '0.999.0' : '../codex-0.154.0';
    f.original.instances[0]!.settings['nativeVersion'] = version; f.target.instances[0]!.settings['nativeVersion'] = version;
  }
  if (mutation === 'changed-file') await writeFile(join(f.newHome, 'sessions/paginated-selected.jsonl'), 'changed');
  const error = ['checksum', 'trigger', 'alternate', 'unknown-version', 'path-version'].includes(mutation) ? 'unsupported_storage' : mutation === 'version' ? 'target_conflict' : mutation === 'changed-file' ? 'backup_changed' : 'backup_invalid';
  const sourceBefore = await inventoryFiles(f.original.runtimeRoot);
  await assert.rejects(relocateNativeStorage(f.original, f.target, f.manifest, distribution), code(error));
  assert.equal(await exists(join(f.target.runtimeRoot, 'native-recovery.json')), false);
  assert.deepEqual(await inventoryFiles(f.original.runtimeRoot), sourceBefore);
});

test('native restore cannot silently change the relative home or recreate absent metadata', async t => {
  const f = await fixture(t);
  f.target.instances[0]!.settings['nativeHome'] = join(f.newHome, 'different');
  await assert.rejects(relocateNativeStorage(f.original, f.target, f.manifest, distribution), code('target_conflict'));
  f.target.instances[0]!.settings['nativeHome'] = f.newHome;
  for (const file of f.manifest.files.filter(file => /\/state_5\.sqlite(?:-wal|-shm)?$/.test(file.path))) await unlink(join(f.target.runtimeRoot, file.path.slice(6)));
  f.manifest.files = f.manifest.files.filter(file => !/\/state_5\.sqlite(?:-wal|-shm)?$/.test(file.path));
  const report = await relocateNativeStorage(f.original, f.target, f.manifest, distribution);
  assert.deepEqual(report.instances, []); assert.equal(await exists(join(f.newHome, 'state_5.sqlite')), false);
});
