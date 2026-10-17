import { DatabaseSync } from 'node:sqlite';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { hasUnresolvedOutcome, canonical, deriveOperationId, hashJson, IvyError, parseOperationId, scopedOperationId } from '../../../packages/sdk/src/node.js';
import type { Operation, RpcClient, Wire } from '../../../packages/sdk/src/node.js';
import { contractVersion, need, readableVersions, same, validate } from './schema.js';
import type { Pin, Values, Destination } from './schema.js';

export interface Document<K extends keyof Values> { pin: Pin; value: Values[K]; writtenAt?: string }
const definitions = { 'secretary/item': 'Item', 'secretary/source': 'Source', 'secretary/operation': 'Accepted',
  'secretary/configuration': 'SecretaryConfiguration', 'secretary/assignment': 'Assignment',
  'secretary/schedule-progress': 'ScheduleProgress', 'secretary/event-progress': 'EventProgress', 'secretary/execution': 'Execution' } as const;
const localKeys = new Set<keyof Values>(['secretary/source', 'secretary/operation']);

function recordReferences<K extends keyof Values>(key: K, value: Values[K]): Record<string, Pin> {
  if (key === 'secretary/item')
    return Object.fromEntries((value as Values['secretary/item']).media.flatMap((entry, index) => entry.pin ? [['media-' + index, entry.pin] as const] : []));
  if (key === 'secretary/execution') {
    const execution = value as Values['secretary/execution'];
    const payload = execution.trigger.payload as Record<string, Wire.Json>;
    return {
      assignment: execution.assignment,
      ...(payload?.['kind'] === 'secretary.object.batch' && typeof payload['objectId'] === 'string' &&
        typeof payload['revision'] === 'number' && typeof payload['beforeRevision'] === 'number'
        ? { before: { objectId: payload['objectId'], revision: payload['beforeRevision'] },
          input: { objectId: payload['objectId'], revision: payload['revision'] } }
        : {}),
    };
  }
  if (key === 'secretary/schedule-progress' || key === 'secretary/event-progress')
    return { assignment: (value as Values['secretary/schedule-progress' | 'secretary/event-progress']).assignment };
  return {};
}

function operationIdentity(value: unknown): string | null {
  if (typeof value === 'string') { try { return parseOperationId(value).issuedAtUnixMs >= 1_000_000_000_000 ? value : null; } catch { return null; } }
  if (Array.isArray(value)) for (const part of value) { const found = operationIdentity(part); if (found) return found; }
  if (value && typeof value === 'object') for (const part of Object.values(value)) { const found = operationIdentity(part); if (found) return found; }
  return null;
}
export const mutation = (...parts: unknown[]) => {
  const parent = operationIdentity(parts);
  return parent ? deriveOperationId(parent, parts) : 'secretary:' + hashJson(parts).slice(7);
};

/** One service-owned journal for source cursors and bounded operation recovery. */
export class SecretaryStore {
  readonly db: DatabaseSync;
  constructor(readonly client: RpcClient, readonly root: string, dataRoot?: string,
    readonly limits = { count: 10_000, bytes: 256 * 1024 * 1024 }) {
    this.db = new DatabaseSync(dataRoot ? join(dataRoot, 'secretary.sqlite') : ':memory:');
    this.db.exec(`PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;
      CREATE TABLE IF NOT EXISTS documents(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,current_revision INTEGER NOT NULL,created_at INTEGER NOT NULL,completed_at INTEGER,value_json TEXT NOT NULL,UNIQUE(key,name));
      CREATE INDEX IF NOT EXISTS documents_pending ON documents(key,completed_at,id);
      CREATE TABLE IF NOT EXISTS revisions(id TEXT NOT NULL,revision INTEGER NOT NULL,value_json TEXT NOT NULL,PRIMARY KEY(id,revision));
      CREATE TABLE IF NOT EXISTS technical_documents(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,current_revision INTEGER NOT NULL,value_json TEXT NOT NULL,UNIQUE(key,name));
      CREATE TABLE IF NOT EXISTS technical_revisions(id TEXT NOT NULL,revision INTEGER NOT NULL,value_json TEXT NOT NULL,PRIMARY KEY(id,revision));
      CREATE TABLE IF NOT EXISTS technical_blobs(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,bytes BLOB NOT NULL,UNIQUE(key,name));
      CREATE TABLE IF NOT EXISTS technical_cleanup(root_id TEXT PRIMARY KEY,delete_after INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS journal_state(key TEXT PRIMARY KEY,value TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS capture_acknowledgements(capture_id TEXT PRIMARY KEY);
      CREATE TABLE IF NOT EXISTS retired_operations(operation_id TEXT PRIMARY KEY,issued_at INTEGER NOT NULL);
      CREATE TRIGGER IF NOT EXISTS retired_operation_insert BEFORE INSERT ON documents
        WHEN NEW.key='secretary/operation' AND EXISTS(SELECT 1 FROM retired_operations WHERE operation_id=json_extract(NEW.value_json,'$.operationId'))
        BEGIN SELECT RAISE(ABORT,'operation_expired'); END;
      CREATE TRIGGER IF NOT EXISTS retired_technical_operation_insert BEFORE INSERT ON technical_documents
        WHEN EXISTS(SELECT 1 FROM retired_operations WHERE operation_id=COALESCE(json_extract(NEW.value_json,'$.operationId'),json_extract(NEW.value_json,'$.request.operationId')))
        BEGIN SELECT RAISE(ABORT,'operation_expired'); END;
      INSERT OR IGNORE INTO capture_acknowledgements SELECT json_extract(c.value,'$.objectId')
        FROM documents d,json_each(d.value_json,'$.checkpointCaptures') c
        WHERE d.key='secretary/operation' AND json_extract(d.value_json,'$.action')='checkpoint'
          AND json_extract(d.value_json,'$.phase')='succeeded';`);
  }
  private localRow(key: keyof Values, selected: Pin | string): Document<any> {
    const id = typeof selected === 'string' ? selected : selected.objectId;
    const head = this.db.prepare('SELECT * FROM documents WHERE id=? AND key=?').get(id, key); need(head, 'not_found', 'The local Secretary record does not exist.');
    const revision = typeof selected === 'string' ? Number(head['current_revision']) : selected.revision;
    const row = this.db.prepare('SELECT value_json FROM revisions WHERE id=? AND revision=?').get(id, revision); need(row, 'not_found', 'The local Secretary revision does not exist.');
    const value = JSON.parse(String(row['value_json'])); validate(definitions[key], value);
    return { pin: { objectId: id, revision }, value };
  }
  private usage(): { count: number; bytes: number } {
    const row = this.db.prepare(`SELECT (SELECT COUNT(*) FROM revisions)+(SELECT COUNT(*) FROM technical_revisions)+(SELECT COUNT(*) FROM technical_blobs)+
      (SELECT COUNT(*) FROM documents WHERE key='secretary/operation' AND completed_at IS NULL) AS count,
      COALESCE((SELECT SUM(length(CAST(value_json AS BLOB))) FROM revisions),0)+COALESCE((SELECT SUM(length(CAST(value_json AS BLOB))) FROM technical_revisions),0)+COALESCE((SELECT SUM(length(bytes)) FROM technical_blobs),0)+
      COALESCE((SELECT SUM(length(CAST(value_json AS BLOB))+512) FROM documents WHERE key='secretary/operation' AND completed_at IS NULL),0) AS bytes`).get()!;
    return { count: Number(row['count']), bytes: Number(row['bytes']) };
  }
  private capacity(extra: number, protectedId?: string, incoming?: unknown, extraCount = 1): void {
    let usage = this.usage();
    while (usage.count + extraCount > this.limits.count || usage.bytes + extra > this.limits.bytes) {
      const before = usage;
      this.cleanup(Date.now(), extra, extraCount, protectedId, incoming);
      usage = this.usage();
      if (usage.count >= before.count && usage.bytes >= before.bytes) break;
    }
    need(usage.count + extraCount <= this.limits.count && usage.bytes + extra <= this.limits.bytes,
      'capacity_exceeded', 'Secretary journal capacity is occupied by live or unresolved work.');
  }
  private fence(value: unknown): void {
    const id = operationIdentity(value);
    if (!id) return;
    const boundary = Math.max(this.expiredBefore(), parseOperationId(id).issuedAtUnixMs);
    this.db.prepare('INSERT OR IGNORE INTO retired_operations VALUES(?,?)').run(id, parseOperationId(id).issuedAtUnixMs);
    this.db.prepare("INSERT INTO journal_state(key,value) VALUES('expiredBefore',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").run(String(boundary));
    this.db.prepare('DELETE FROM retired_operations WHERE issued_at<?').run(boundary);
  }
  cleanup(_now = Date.now(), extraBytes = 0, extraCount = 0, protectedId?: string, incoming?: unknown): void {
    const work = () => {
      const needsRoom = () => { const usage = this.usage(); return usage.count + extraCount > this.limits.count || usage.bytes + extraBytes > this.limits.bytes; };
      if (!needsRoom()) return;
      this.db.prepare(`INSERT OR IGNORE INTO technical_cleanup SELECT id,0 FROM technical_documents
        WHERE key IN ('secretary/public-operation','secretary/follow-up') AND json_extract(value_json,'$.phase')='succeeded'`).run();
      const operations = this.db.prepare("SELECT id,value_json,completed_at FROM documents WHERE key='secretary/operation' AND completed_at IS NOT NULL ORDER BY completed_at,rowid").all();
      const documents = this.db.prepare('SELECT id,key,value_json FROM technical_documents').all()
        .map(row => ({ id: String(row['id']), key: String(row['key']), value: JSON.parse(String(row['value_json'])) as Record<string, unknown> }));
      const available = new Set([...operations.map(row => String(row['id'])), ...documents.map(row => row.id),
        ...this.db.prepare('SELECT id FROM technical_blobs').all().map(row => String(row['id']))]);
      const references = (value: unknown, found = new Set<string>()): Set<string> => {
        if (typeof value === 'string' && available.has(value)) found.add(value);
        else if (Array.isArray(value)) for (const part of value) references(part, found);
        else if (value && typeof value === 'object') for (const part of Object.values(value)) references(part, found);
        return found;
      };
      const protectedOperations = new Set<string>(protectedId ? [protectedId] : []);
      for (const id of references(incoming)) protectedOperations.add(id);
      for (const document of documents) for (const id of references(document.value)) protectedOperations.add(id);
      for (const row of this.db.prepare("SELECT value_json FROM documents WHERE key='secretary/operation' AND completed_at IS NULL").all())
        for (const id of references(JSON.parse(String(row['value_json'])))) protectedOperations.add(id);
      const roots = this.db.prepare('SELECT root_id,delete_after FROM technical_cleanup ORDER BY delete_after,rowid').all();
      const candidates = [...operations.map(row => ({ kind: 'operation', id: String(row['id']), at: Number(row['completed_at']), row })),
        ...roots.map(row => ({ kind: 'technical', id: String(row['root_id']), at: Number(row['delete_after']), row }))].sort((a, b) => a.at - b.at);
      for (const candidate of candidates) {
        if (candidate.id === protectedId) continue;
        if (candidate.kind === 'operation') {
          const value = JSON.parse(String(candidate.row['value_json'])) as Values['secretary/operation'];
          if (!['succeeded', 'failed'].includes(value.phase) || protectedOperations.has(candidate.id) || hasUnresolvedOutcome(value)) continue;
          if (value.action === 'capture' && value.phase === 'succeeded' &&
            !this.db.prepare('SELECT 1 FROM capture_acknowledgements WHERE capture_id=?').get(candidate.id)) continue;
          if (!operationIdentity(value.operationId)) continue;
          this.fence(value.operationId);
          this.db.prepare('DELETE FROM revisions WHERE id=?').run(candidate.id);
          this.db.prepare('DELETE FROM documents WHERE id=?').run(candidate.id);
          this.db.prepare('DELETE FROM capture_acknowledgements WHERE capture_id=?').run(candidate.id);
          if (!needsRoom()) break;
          continue;
        }
        const selected = new Set([candidate.id]);
        for (let changed = true; changed;) {
          changed = false;
          for (const document of documents) {
            if (document.key === 'secretary/triage-thread') continue;
            const found = references(document.value);
            if (selected.has(document.id) || [...found].some(id => selected.has(id))) {
              for (const id of [document.id, ...found]) if (!selected.has(id)) { selected.add(id); changed = true; }
            }
          }
        }
        const members = documents.filter(document => selected.has(document.id));
        if (members.some(document => document.key === 'secretary/follow-up-context' &&
          (document.value['threadId'] === null || documents.some(other => other.key === 'secretary/follow-up' &&
            ['prepared', 'running', 'outcome_unknown'].includes(String(other.value['phase'])) &&
            (other.value['request'] as { itemId?: string } | undefined)?.itemId === document.value['itemId'])))) continue;
        if ([...references(incoming), protectedId ?? ''].some(id => selected.has(id)) || members.some(document => hasUnresolvedOutcome(document.value) ||
          ['accepted', 'dispatched', 'prepared', 'running', 'pending', 'archiving'].includes(String(document.value['phase'])))) continue;
        if (documents.some(document => !selected.has(document.id) && [...references(document.value)].some(id => selected.has(id)))) continue;
        if (this.db.prepare('SELECT id,value_json FROM documents').all().some(row =>
          !selected.has(String(row['id'])) && [...references(JSON.parse(String(row['value_json'])))].some(id => selected.has(id)))) continue;
        for (const document of members) this.fence(document.value['operationId'] ?? (document.value['request'] as Record<string, unknown> | undefined)?.['operationId']);
        for (const id of selected) {
          this.db.prepare('DELETE FROM technical_blobs WHERE id=?').run(id);
          this.db.prepare('DELETE FROM technical_revisions WHERE id=?').run(id);
          this.db.prepare("DELETE FROM technical_documents WHERE id=? AND key<>'secretary/triage-thread'").run(id);
          this.db.prepare('DELETE FROM technical_cleanup WHERE root_id=?').run(id);
        }
        if (!needsRoom()) break;
      }
    };
    if (this.db.isTransaction) work();
    else { this.db.exec('BEGIN IMMEDIATE'); try { work(); this.db.exec('COMMIT'); } catch (error) { this.db.exec('ROLLBACK'); throw error; } }
  }
  expiredBefore(): number { return Number(this.db.prepare("SELECT value FROM journal_state WHERE key='expiredBefore'").get()?.['value'] ?? 0); }
  completeTechnicalWorkflow(rootId: string, now = Date.now()): void {
    this.db.prepare('INSERT INTO technical_cleanup VALUES(?,?) ON CONFLICT(root_id) DO NOTHING').run(rootId, now);
  }
  decode<K extends keyof Values>(key: K, read: Operation.ObjectRead): Document<K> {
    need(!localKeys.has(key) && read.object.parentId === this.root && !read.object.effectivelyArchived && read.object.contractKey === key && readableVersions(key).includes(read.revision.contractVersion) && read.revision.mediaType === 'application/json' && read.content.encoding === 'json', 'secretary_scope_conflict', 'The Secretary record must remain in its original active root and exact contract.');
    need(hashJson(read.content.value) === read.revision.contentHash && Buffer.byteLength(canonical(read.content.value)) === read.revision.byteLength, 'secretary_evidence_conflict', 'The saved record does not match its declared bytes.');
    validate(definitions[key], read.content.value);
    const expectedReferences = recordReferences(key, read.content.value as unknown as Values[K]);
    need(same(read.revision.references, expectedReferences), 'secretary_evidence_conflict', 'The Secretary record references must match its exact revision content.');
    return { pin: { objectId: read.object.id, revision: read.revision.revision }, value: read.content.value as unknown as Values[K], writtenAt: read.revision.createdAt };
  }
  async read<K extends keyof Values>(key: K, selected: Pin | string): Promise<Document<K>> {
    return localKeys.has(key) ? this.localRow(key, selected) : this.decode(key, await this.client.request('objects.read', typeof selected === 'string' ? { objectId: selected } : selected));
  }
  async named<K extends keyof Values>(key: K, name: string): Promise<Document<K> | null> {
    if (localKeys.has(key)) { const row = this.db.prepare('SELECT id FROM documents WHERE key=? AND name=?').get(key, name); return row ? this.read(key, String(row['id'])) : null; }
    const page = await this.client.request('objects.query', { contractKey: key, includeArchived: true, limit: 2,
      where: { op: 'and', args: [{ op: 'eq', field: 'object.parentId', value: this.root }, { op: 'eq', field: 'object.name', value: name }] } });
    need(page.items.length <= 1 && !page.nextCursor, 'secretary_identity_conflict', 'An original Secretary name must be unique.');
    return page.items[0] ? this.read(key, page.items[0].objectId) : null;
  }
  async write<K extends keyof Values>(key: K, value: Values[K], destination: Destination, id: string): Promise<Pin> {
    validate(definitions[key], value); const encoded = canonical(value);
    if (localKeys.has(key)) {
      if ('createName' in destination) {
        const objectId = randomUUID();
        this.db.exec('BEGIN IMMEDIATE'); try {
          const operation = key === 'secretary/operation' ? value as Values['secretary/operation'] : null;
          const pending = operation?.phase === 'accepted', capture = pending && operation.action === 'capture';
          // Reserve the terminal receipt and leave room for the checkpoint that
          // acknowledges captures, even when ordinary admission is full.
          this.capacity(Buffer.byteLength(encoded) * (pending ? 2 : 1) + (pending ? 512 : 0) + (capture ? 65_536 : 0),
            undefined, value, (pending ? 2 : 1) + (capture ? 2 : 0));
          const completed = key === 'secretary/operation' && (value as Values['secretary/operation']).phase !== 'accepted' ? Date.now() : null;
          this.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(objectId, key, destination.createName, 1, Date.now(), completed, encoded);
          this.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(objectId, 1, encoded); this.db.exec('COMMIT'); return { objectId, revision: 1 };
        } catch (error) { this.db.exec('ROLLBACK'); throw error; }
      }
      this.db.exec('BEGIN IMMEDIATE'); try {
        const prior = this.db.prepare('SELECT value_json,completed_at FROM documents WHERE id=?').get(destination.object.objectId);
        const released = key === 'secretary/operation' && prior?.['completed_at'] === null && (value as Values['secretary/operation']).phase !== 'accepted';
        if (key === 'secretary/source') {
          const usage = this.db.prepare('SELECT COUNT(*) AS count,COALESCE(SUM(length(CAST(value_json AS BLOB))),0) AS bytes FROM revisions WHERE id=?').get(destination.object.objectId)!;
          this.capacity(Buffer.byteLength(encoded) - Number(usage['bytes']), destination.object.objectId, value, 1 - Number(usage['count']));
        } else this.capacity(Buffer.byteLength(encoded) - (released ? Buffer.byteLength(String(prior!['value_json'])) + 512 : 0),
          destination.object.objectId, value, released ? 0 : 1);
        const next = destination.object.revision + 1, completed = key === 'secretary/operation' && (value as Values['secretary/operation']).phase !== 'accepted' ? Date.now() : null;
        const changed = this.db.prepare('UPDATE documents SET current_revision=?,value_json=?,completed_at=? WHERE id=? AND key=? AND current_revision=?').run(next, encoded, completed, destination.object.objectId, key, destination.object.revision);
        need(changed.changes === 1, 'revision_conflict', 'The local Secretary row changed.');
        this.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(destination.object.objectId, next, encoded);
        if (key === 'secretary/operation') {
          const operation = value as Values['secretary/operation'];
          if (operation.action === 'checkpoint' && operation.phase === 'succeeded')
            for (const capture of operation.checkpointCaptures) this.db.prepare('INSERT OR IGNORE INTO capture_acknowledgements VALUES(?)').run(capture.objectId);
        }
        // A source cursor is current mutable service state. It has no historical reader.
        if (key === 'secretary/source') this.db.prepare('DELETE FROM revisions WHERE id=? AND revision<>?').run(destination.object.objectId, next);
        this.db.exec('COMMIT'); return { objectId: destination.object.objectId, revision: next };
      } catch (error) { this.db.exec('ROLLBACK'); throw error; }
    }
    return this.writePrepared(this.objectWriteRequest(key, value, destination, id));
  }
  objectWriteRequest<K extends keyof Values>(
    key: K,
    value: Values[K],
    destination: Destination,
    id: string,
  ): Wire.ObjectWrite {
    need(!localKeys.has(key), 'invalid_arguments', 'Local Secretary records do not have Hive write receipts.');
    validate(definitions[key], value);
    parseOperationId(id);
    const references = recordReferences(key, value);
    return { mutationId: id, contractVersion: contractVersion(key), references,
      ...('createName' in destination ? { create: { contractKey: key, parentId: this.root, name: destination.createName, ownerObjectId: null } } : { objectId: destination.object.objectId, expectedRevision: destination.object.revision }),
      content: { encoding: 'json', value: value as unknown as Wire.Json } };
  }
  async writePrepared(request: Wire.ObjectWrite): Promise<Pin> {
    const result = await this.client.request('objects.write', request);
    return { objectId: result.object.id, revision: result.revision.revision };
  }
  async create<K extends keyof Values>(key: K, name: string, value: Values[K], id: string): Promise<Document<K>> {
    const existing = await this.named(key, name); if (existing) return existing;
    try { return this.read(key, await this.write(key, value, { createName: name }, id)); }
    catch (error) { const winner = await this.named(key, name); if (!winner) throw error; return winner; }
  }
  async amend<K extends keyof Values>(current: Document<K>, key: K, value: Values[K]): Promise<Document<K>> {
    if (same(current.value, value)) return current;
    const operationId = await scopedOperationId(this.client, ['secretary-amend', this.root, key, current.pin, hashJson(value)]);
    try { return this.read(key, await this.write(key, value, { object: current.pin }, operationId)); }
    catch (error) {
      if (!(error instanceof IvyError) || !['revision_conflict', 'mutation_conflict'].includes(error.code)) throw error;
      const saved = await this.read(key, current.pin.objectId); if (same(saved.value, value)) return saved; throw error;
    }
  }
  async original<K extends keyof Values>(key: K, current: Document<K>): Promise<Document<K>> { return current.pin.revision === 1 ? current : this.read(key, { objectId: current.pin.objectId, revision: 1 }); }
  localList<K extends keyof Values>(key: K, predicate?: (value: Values[K]) => boolean): Document<K>[] {
    return this.db.prepare('SELECT id FROM documents WHERE key=? ORDER BY created_at,id').all(key).map(row => this.localRow(key, String(row['id'])) as Document<K>).filter(row => !predicate || predicate(row.value));
  }
  acceptedIds(limit: number, afterId?: string): string[] {
    return this.db.prepare("SELECT id FROM documents WHERE key='secretary/operation' AND completed_at IS NULL AND id>? ORDER BY id LIMIT ?")
      .all(afterId ?? '', limit).map(row => String(row['id']));
  }
  technicalRead<T>(key: string, selected: Pin | string): { pin: Pin; name: string; value: T } {
    const id = typeof selected === 'string' ? selected : selected.objectId, head = this.db.prepare('SELECT * FROM technical_documents WHERE id=? AND key=?').get(id, key); need(head, 'not_found', 'The local workflow record does not exist.');
    const revision = typeof selected === 'string' ? Number(head['current_revision']) : selected.revision, row = this.db.prepare('SELECT value_json FROM technical_revisions WHERE id=? AND revision=?').get(id, revision); need(row, 'not_found', 'The local workflow revision does not exist.');
    return { pin: { objectId: id, revision }, name: String(head['name']), value: JSON.parse(String(row['value_json'])) as T };
  }
  technicalNamed<T>(key: string, name: string): { pin: Pin; name: string; value: T } | null {
    const row = this.db.prepare('SELECT id FROM technical_documents WHERE key=? AND name=?').get(key, name); return row ? this.technicalRead<T>(key, String(row['id'])) : null;
  }
  technicalList<T>(key: string): { pin: Pin; name: string; value: T }[] {
    return this.db.prepare('SELECT id FROM technical_documents WHERE key=? ORDER BY id').all(key).map(row => this.technicalRead<T>(key, String(row['id'])));
  }
  technicalDelete(key: string, id: string): void {
    this.db.exec('BEGIN IMMEDIATE');
    try {
      this.db.prepare('DELETE FROM technical_revisions WHERE id=?').run(id);
      this.db.prepare('DELETE FROM technical_documents WHERE id=? AND key=?').run(id, key);
      this.db.exec('COMMIT');
    } catch (error) { this.db.exec('ROLLBACK'); throw error; }
  }
  technicalCreate<T>(key: string, name: string, value: T): { pin: Pin; name: string; value: T } {
    const prior = this.technicalNamed<T>(key, name); if (prior) return prior; const encoded = canonical(value); const id = randomUUID();
    this.db.exec('BEGIN IMMEDIATE'); try { this.capacity(Buffer.byteLength(encoded), undefined, value); this.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(id, key, name, 1, encoded); this.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(id, 1, encoded); this.db.exec('COMMIT'); }
    catch (error) { this.db.exec('ROLLBACK'); const winner = this.technicalNamed<T>(key, name); if (winner) return winner; throw error; }
    return { pin: { objectId: id, revision: 1 }, name, value: structuredClone(value) };
  }
  technicalAmend<T>(key: string, current: { pin: Pin }, value: T): { pin: Pin; name: string; value: T } {
    const encoded = canonical(value); const next = current.pin.revision + 1; this.db.exec('BEGIN IMMEDIATE');
    try {
      const usage = this.db.prepare('SELECT COUNT(*) AS count,COALESCE(SUM(length(CAST(value_json AS BLOB))),0) AS bytes FROM technical_revisions WHERE id=?').get(current.pin.objectId)!;
      this.capacity(Buffer.byteLength(encoded) - Number(usage['bytes']), current.pin.objectId, value, 1 - Number(usage['count']));
      const changed = this.db.prepare('UPDATE technical_documents SET current_revision=?,value_json=? WHERE id=? AND key=? AND current_revision=?').run(next, encoded, current.pin.objectId, key, current.pin.revision);
      need(changed.changes === 1, 'revision_conflict', 'The local workflow row changed.');
      this.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(current.pin.objectId, next, encoded);
      this.db.prepare('DELETE FROM technical_revisions WHERE id=? AND revision<>?').run(current.pin.objectId, next);
      this.db.exec('COMMIT');
    }
    catch (error) { this.db.exec('ROLLBACK'); throw error; }
    return this.technicalRead<T>(key, { objectId: current.pin.objectId, revision: next });
  }
  technicalBlob(key: string, name: string, pin?: Pin): { pin: Pin; bytes: Buffer } | null {
    const row = pin ? this.db.prepare('SELECT * FROM technical_blobs WHERE id=? AND key=?').get(pin.objectId, key) : this.db.prepare('SELECT * FROM technical_blobs WHERE key=? AND name=?').get(key, name); if (!row) return null;
    need(!pin || pin.revision === 1, 'secretary_evidence_conflict', 'The local blob revision is invalid.'); return { pin: { objectId: String(row['id']), revision: 1 }, bytes: Buffer.from(row['bytes'] as Uint8Array) };
  }
  technicalSaveBlob(key: string, name: string, bytes: Buffer): Pin {
    const prior = this.technicalBlob(key, name); if (prior) { need(prior.bytes.equals(bytes), 'secretary_identity_conflict', 'The local blob identity has different bytes.'); return prior.pin; } const id = randomUUID();
    this.db.exec('BEGIN IMMEDIATE');
    try { this.capacity(bytes.length); this.db.prepare('INSERT INTO technical_blobs VALUES(?,?,?,?)').run(id, key, name, bytes); this.db.exec('COMMIT'); }
    catch (error) { this.db.exec('ROLLBACK'); const winner = this.technicalBlob(key, name); if (winner?.bytes.equals(bytes)) return winner.pin; throw error; } return { objectId: id, revision: 1 };
  }
  close(): void { this.db.close(); }
}
export const conflict = (error: unknown) => error instanceof IvyError && error.outcome === 'not_executed' && ['revision_conflict', 'mutation_conflict'].includes(error.code);
