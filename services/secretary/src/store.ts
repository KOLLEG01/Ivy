import { DatabaseSync } from 'node:sqlite';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { canonical, deriveOperationId, hashJson, IvyError, parseOperationId, scopedOperationId } from '../../../packages/sdk/src/node.js';
import type { Operation, RpcClient, Wire } from '../../../packages/sdk/src/node.js';
import { contractVersion, need, readableVersions, same, validate } from './schema.js';
import type { Pin, Values, Destination } from './schema.js';

export interface Document<K extends keyof Values> { pin: Pin; value: Values[K]; writtenAt?: string }
const definitions = { 'secretary/item': 'Item', 'secretary/source': 'Source', 'secretary/operation': 'Accepted',
  'secretary/configuration': 'SecretaryConfiguration', 'secretary/assignment': 'Assignment',
  'secretary/schedule-progress': 'ScheduleProgress', 'secretary/event-progress': 'EventProgress', 'secretary/execution': 'Execution' } as const;
const localKeys = new Set<keyof Values>(['secretary/source', 'secretary/operation']);

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
  private lastCleanupAt = 0;
  constructor(readonly client: RpcClient, readonly root: string, dataRoot?: string) {
    this.db = new DatabaseSync(dataRoot ? join(dataRoot, 'secretary.sqlite') : ':memory:');
    this.db.exec(`PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;
      CREATE TABLE IF NOT EXISTS documents(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,current_revision INTEGER NOT NULL,created_at INTEGER NOT NULL,completed_at INTEGER,value_json TEXT NOT NULL,UNIQUE(key,name));
      CREATE INDEX IF NOT EXISTS documents_pending ON documents(key,completed_at,id);
      CREATE TABLE IF NOT EXISTS revisions(id TEXT NOT NULL,revision INTEGER NOT NULL,value_json TEXT NOT NULL,PRIMARY KEY(id,revision));
      CREATE TABLE IF NOT EXISTS technical_documents(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,current_revision INTEGER NOT NULL,value_json TEXT NOT NULL,UNIQUE(key,name));
      CREATE TABLE IF NOT EXISTS technical_revisions(id TEXT NOT NULL,revision INTEGER NOT NULL,value_json TEXT NOT NULL,PRIMARY KEY(id,revision));
      CREATE TABLE IF NOT EXISTS technical_blobs(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,bytes BLOB NOT NULL,UNIQUE(key,name));
      CREATE TABLE IF NOT EXISTS technical_cleanup(root_id TEXT PRIMARY KEY,delete_after INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS journal_state(key TEXT PRIMARY KEY,value TEXT NOT NULL);`);
  }
  private localRow(key: keyof Values, selected: Pin | string): Document<any> {
    const id = typeof selected === 'string' ? selected : selected.objectId;
    const head = this.db.prepare('SELECT * FROM documents WHERE id=? AND key=?').get(id, key); need(head, 'not_found', 'The local Secretary record does not exist.');
    const revision = typeof selected === 'string' ? Number(head['current_revision']) : selected.revision;
    const row = this.db.prepare('SELECT value_json FROM revisions WHERE id=? AND revision=?').get(id, revision); need(row, 'not_found', 'The local Secretary revision does not exist.');
    const value = JSON.parse(String(row['value_json'])); validate(definitions[key], value);
    return { pin: { objectId: id, revision }, value };
  }
  private capacity(extra: number): void {
    const now = Date.now();
    if (now - this.lastCleanupAt >= 60_000) this.cleanup(now);
    const query = () => this.db.prepare('SELECT (SELECT COUNT(*) FROM revisions)+(SELECT COUNT(*) FROM technical_revisions)+(SELECT COUNT(*) FROM technical_blobs) AS count,COALESCE((SELECT SUM(length(value_json)) FROM revisions),0)+COALESCE((SELECT SUM(length(value_json)) FROM technical_revisions),0)+COALESCE((SELECT SUM(length(bytes)) FROM technical_blobs),0) AS bytes').get()!;
    let totals = query();
    if (Number(totals['count']) >= 10_000 || Number(totals['bytes']) + extra > 256 * 1024 * 1024) { this.cleanup(now); totals = query(); }
    need(Number(totals['count']) < 10_000 && Number(totals['bytes']) + extra <= 256 * 1024 * 1024, 'capacity_exceeded', 'The Secretary workflow journal is full.');
  }
  cleanup(now = Date.now()): void {
    const cutoff = now - 24 * 60 * 60 * 1000;
    this.db.exec('BEGIN IMMEDIATE');
    try {
      const ids = this.db.prepare("SELECT id,value_json,completed_at FROM documents WHERE key='secretary/operation' AND completed_at<=?").all(cutoff)
        .filter(row => {
          const completedAt = Number(row['completed_at']);
          try {
            const value = JSON.parse(String(row['value_json'])) as Values['secretary/operation'];
            return now >= Math.max(parseOperationId(value.operationId).issuedAtUnixMs + 24 * 60 * 60 * 1000, completedAt + 24 * 60 * 60 * 1000);
          } catch { return completedAt <= cutoff; }
        }).map(row => String(row['id']));
      for (const id of ids) { this.db.prepare('DELETE FROM revisions WHERE id=?').run(id); this.db.prepare('DELETE FROM documents WHERE id=?').run(id); }
      const technical = this.db.prepare('SELECT root_id FROM technical_cleanup WHERE delete_after<=?').all(now).map(row => String(row['root_id']));
      if (technical.length) {
        const documents = this.db.prepare("SELECT id,key,value_json FROM technical_documents WHERE key<>'secretary/triage-thread'").all()
          .map(row => ({ id: String(row['id']), key: String(row['key']), value: JSON.parse(String(row['value_json'])) as unknown }));
        const available = new Set([...documents.map(row => row.id), ...this.db.prepare('SELECT id FROM technical_blobs').all().map(row => String(row['id']))]);
        const references = (value: unknown, found: Set<string>): void => {
          if (typeof value === 'string') { if (available.has(value)) found.add(value); return; }
          if (Array.isArray(value)) { for (const part of value) references(part, found); return; }
          if (value && typeof value === 'object') for (const part of Object.values(value)) references(part, found);
        };
        for (const root of technical) {
          const selected = new Set([root]);
          for (let changed = true; changed;) {
            changed = false;
            for (const document of documents) {
              const found = new Set<string>(); references(document.value, found);
              if (selected.has(document.id) || [...found].some(id => selected.has(id))) {
                if (!selected.has(document.id)) { selected.add(document.id); changed = true; }
                for (const id of found) if (!selected.has(id)) { selected.add(id); changed = true; }
              }
            }
          }
          for (const id of selected) { this.db.prepare('DELETE FROM technical_blobs WHERE id=?').run(id); this.db.prepare('DELETE FROM technical_revisions WHERE id=?').run(id); this.db.prepare("DELETE FROM technical_documents WHERE id=? AND key<>'secretary/triage-thread'").run(id); }
          this.db.prepare('DELETE FROM technical_cleanup WHERE root_id=?').run(root);
        }
      }
      for (const key of ['secretary/follow-up-context', 'secretary/follow-up', 'secretary/public-operation', 'secretary/assignment-handoff', 'secretary/assignment-voice', 'secretary/notice-retry', 'secretary/execution-retry', 'secretary/browser-notice']) {
        const expired = this.db.prepare('SELECT id,value_json FROM technical_documents WHERE key=?').all(key)
          .filter(row => {
            try { const expiresAt = (JSON.parse(String(row['value_json'])) as { expiresAt?: unknown }).expiresAt;
              return typeof expiresAt === 'number' && Number.isFinite(expiresAt) && expiresAt <= now; }
            catch { return false; }
          }).map(row => String(row['id']));
        for (const id of expired) {
          this.db.prepare('DELETE FROM technical_revisions WHERE id=?').run(id);
          this.db.prepare('DELETE FROM technical_documents WHERE id=? AND key=?').run(id, key);
        }
      }
      const previous = Number(this.db.prepare("SELECT value FROM journal_state WHERE key='expiredBefore'").get()?.['value'] ?? 0);
      this.db.prepare("INSERT INTO journal_state(key,value) VALUES('expiredBefore',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").run(String(Math.max(previous, cutoff)));
      this.db.exec('COMMIT'); this.lastCleanupAt = now;
    } catch (error) { this.db.exec('ROLLBACK'); throw error; }
  }
  expiredBefore(): number { return Number(this.db.prepare("SELECT value FROM journal_state WHERE key='expiredBefore'").get()?.['value'] ?? 0); }
  completeTechnicalWorkflow(rootId: string, now = Date.now()): void {
    this.db.prepare('INSERT INTO technical_cleanup VALUES(?,?) ON CONFLICT(root_id) DO NOTHING').run(rootId, now + 24 * 60 * 60 * 1000);
  }
  decode<K extends keyof Values>(key: K, read: Operation.ObjectRead): Document<K> {
    need(!localKeys.has(key) && read.object.parentId === this.root && !read.object.effectivelyArchived && read.object.contractKey === key && readableVersions(key).includes(read.revision.contractVersion) && read.revision.mediaType === 'application/json' && read.content.encoding === 'json', 'secretary_scope_conflict', 'The Secretary record must remain in its original active root and exact contract.');
    need(hashJson(read.content.value) === read.revision.contentHash && Buffer.byteLength(canonical(read.content.value)) === read.revision.byteLength, 'secretary_evidence_conflict', 'The saved record does not match its declared bytes.');
    validate(definitions[key], read.content.value);
    const expectedReferences = key === 'secretary/item'
      ? Object.fromEntries((read.content.value as unknown as Values['secretary/item']).media.flatMap((entry, index) => entry.pin ? [['media-' + index, entry.pin] as const] : []))
      : key === 'secretary/execution'
        ? { assignment: (read.content.value as unknown as Values['secretary/execution']).assignment }
        : key === 'secretary/schedule-progress' || key === 'secretary/event-progress'
          ? { assignment: (read.content.value as unknown as Values['secretary/schedule-progress' | 'secretary/event-progress']).assignment }
        : {};
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
      this.capacity(Buffer.byteLength(encoded));
      if ('createName' in destination) {
        const objectId = randomUUID();
        this.db.exec('BEGIN IMMEDIATE'); try {
          const completed = key === 'secretary/operation' && (value as Values['secretary/operation']).phase !== 'accepted' ? Date.now() : null;
          this.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(objectId, key, destination.createName, 1, Date.now(), completed, encoded);
          this.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(objectId, 1, encoded); this.db.exec('COMMIT'); return { objectId, revision: 1 };
        } catch (error) { this.db.exec('ROLLBACK'); throw error; }
      }
      this.db.exec('BEGIN IMMEDIATE'); try {
        const next = destination.object.revision + 1, completed = key === 'secretary/operation' && (value as Values['secretary/operation']).phase !== 'accepted' ? Date.now() : null;
        const changed = this.db.prepare('UPDATE documents SET current_revision=?,value_json=?,completed_at=? WHERE id=? AND key=? AND current_revision=?').run(next, encoded, completed, destination.object.objectId, key, destination.object.revision);
        need(changed.changes === 1, 'revision_conflict', 'The local Secretary row changed.');
        this.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(destination.object.objectId, next, encoded);
        // A source cursor is current mutable service state. It has no historical reader.
        if (key === 'secretary/source') this.db.prepare('DELETE FROM revisions WHERE id=? AND revision<>?').run(destination.object.objectId, next);
        this.db.exec('COMMIT'); return { objectId: destination.object.objectId, revision: next };
      } catch (error) { this.db.exec('ROLLBACK'); throw error; }
    }
    return this.writePrepared(this.objectWriteRequest(key, value, destination, id));
  }
  objectWriteRequest<K extends keyof Values>(key: K, value: Values[K], destination: Destination, id: string): Wire.ObjectWrite {
    need(!localKeys.has(key), 'invalid_arguments', 'Local Secretary records do not have Hive write receipts.');
    validate(definitions[key], value); parseOperationId(id);
    const references = key === 'secretary/item'
      ? Object.fromEntries((value as Values['secretary/item']).media.flatMap((entry, index) => entry.pin ? [['media-' + index, entry.pin] as const] : []))
      : key === 'secretary/execution' ? { assignment: (value as Values['secretary/execution']).assignment }
        : key === 'secretary/schedule-progress' || key === 'secretary/event-progress'
          ? { assignment: (value as Values['secretary/schedule-progress' | 'secretary/event-progress']).assignment } : {};
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
    const prior = this.technicalNamed<T>(key, name); if (prior) return prior; const encoded = canonical(value); this.capacity(Buffer.byteLength(encoded)); const id = randomUUID();
    this.db.exec('BEGIN IMMEDIATE'); try { this.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(id, key, name, 1, encoded); this.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(id, 1, encoded); this.db.exec('COMMIT'); }
    catch (error) { this.db.exec('ROLLBACK'); const winner = this.technicalNamed<T>(key, name); if (winner) return winner; throw error; }
    return { pin: { objectId: id, revision: 1 }, name, value: structuredClone(value) };
  }
  technicalAmend<T>(key: string, current: { pin: Pin }, value: T): { pin: Pin; name: string; value: T } {
    const encoded = canonical(value); this.capacity(Buffer.byteLength(encoded)); const next = current.pin.revision + 1; this.db.exec('BEGIN IMMEDIATE');
    try { const changed = this.db.prepare('UPDATE technical_documents SET current_revision=?,value_json=? WHERE id=? AND key=? AND current_revision=?').run(next, encoded, current.pin.objectId, key, current.pin.revision); need(changed.changes === 1, 'revision_conflict', 'The local workflow row changed.'); this.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(current.pin.objectId, next, encoded); this.db.prepare('DELETE FROM technical_revisions WHERE id=? AND revision<>?').run(current.pin.objectId, next); this.db.exec('COMMIT'); }
    catch (error) { this.db.exec('ROLLBACK'); throw error; }
    return this.technicalRead<T>(key, { objectId: current.pin.objectId, revision: next });
  }
  technicalBlob(key: string, name: string, pin?: Pin): { pin: Pin; bytes: Buffer } | null {
    const row = pin ? this.db.prepare('SELECT * FROM technical_blobs WHERE id=? AND key=?').get(pin.objectId, key) : this.db.prepare('SELECT * FROM technical_blobs WHERE key=? AND name=?').get(key, name); if (!row) return null;
    need(!pin || pin.revision === 1, 'secretary_evidence_conflict', 'The local blob revision is invalid.'); return { pin: { objectId: String(row['id']), revision: 1 }, bytes: Buffer.from(row['bytes'] as Uint8Array) };
  }
  technicalSaveBlob(key: string, name: string, bytes: Buffer): Pin {
    const prior = this.technicalBlob(key, name); if (prior) { need(prior.bytes.equals(bytes), 'secretary_identity_conflict', 'The local blob identity has different bytes.'); return prior.pin; } this.capacity(bytes.length); const id = randomUUID();
    try { this.db.prepare('INSERT INTO technical_blobs VALUES(?,?,?,?)').run(id, key, name, bytes); }
    catch (error) { const winner = this.technicalBlob(key, name); if (winner?.bytes.equals(bytes)) return winner.pin; throw error; } return { objectId: id, revision: 1 };
  }
  close(): void { this.db.close(); }
}
export const conflict = (error: unknown) => error instanceof IvyError && error.outcome === 'not_executed' && ['revision_conflict', 'mutation_conflict'].includes(error.code);
