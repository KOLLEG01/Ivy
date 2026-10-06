import { DatabaseSync } from 'node:sqlite';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { canonical, hashJson } from '../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../packages/sdk/src/node.js';
import { parseOperationId } from '../../../packages/sdk/src/node.js';
import type { Chat, Operation, Wire } from '../../../packages/sdk/src/node.js';
import { scopedOperationId } from '../../../packages/sdk/src/client.js';
import type { RpcClient } from '../../../packages/sdk/src/client.js';
import { localContractKeys, version, readableVersions } from './schema.js';
import type { ContractKey, ContractValues } from './schema.js';

export interface Document<K extends ContractKey> {
  key: K; pin: Chat.ObjectPin; value: ContractValues[K]; metadata: Operation.ObjectMetadata; revision: Operation.RevisionMetadata;
}
export type Destination = { create: { parentId: string | null; name: string } } | { objectId: string; expectedRevision: number };
export const mutation = (scope: string, step: string) => 'chat:' + hashJson({ scope, step }).slice(7);
export const pinOf = (value: Operation.ObjectRead | Operation.ObjectWriteResult): Chat.ObjectPin => ({ objectId: value.object.id, revision: value.revision.revision });

/** Exact Hive persistence; service admission and domain transitions remain the caller's responsibility. */
export class ChatStore {
  changeVersion = 0;
  private readonly local: DatabaseSync;
  private runtimeEpoch: string | null = null;
  private lastMaintenance=0;
  constructor(readonly client: RpcClient, readonly rootObjectId: string | null, dataRoot?: string) {
    this.local = new DatabaseSync(dataRoot ? join(dataRoot, 'chat-bridge.sqlite') : ':memory:');
    this.local.exec(`PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;
      CREATE TABLE IF NOT EXISTS documents(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,parent_id TEXT,current_revision INTEGER NOT NULL,created_at TEXT NOT NULL,updated_at TEXT NOT NULL,UNIQUE(key,name,parent_id));
      CREATE TABLE IF NOT EXISTS revisions(id TEXT NOT NULL,revision INTEGER NOT NULL,value_json TEXT NOT NULL,created_at TEXT NOT NULL,PRIMARY KEY(id,revision));
      CREATE TABLE IF NOT EXISTS blobs(id TEXT PRIMARY KEY,key TEXT NOT NULL,name TEXT NOT NULL,parent_id TEXT,bytes BLOB NOT NULL,UNIQUE(key,name,parent_id));
      CREATE TABLE IF NOT EXISTS markers(key TEXT PRIMARY KEY,value_json TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS workflow_cleanup(root_id TEXT PRIMARY KEY,delete_after INTEGER NOT NULL);
      CREATE UNIQUE INDEX IF NOT EXISTS documents_identity ON documents(key,name,COALESCE(parent_id,''));
      CREATE UNIQUE INDEX IF NOT EXISTS blobs_identity ON blobs(key,name,COALESCE(parent_id,''));`);
  }
  setRuntimeEpoch(value: string): void { requireThat(value.length > 0 && !value.includes(':'), 'invalid_arguments', 'Invalid runtime epoch.'); this.runtimeEpoch = value; this.expireOperations(); }
  rememberNoticeInput(inputId: string): void {
    if (this.isNoticeInput(mutation(inputId, 'native-user-message'))) return;
    this.local.prepare('INSERT INTO markers VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json')
      .run('notice-input:' + mutation(inputId, 'native-user-message'), canonical(Date.now() + 7 * 86_400_000));
  }
  isNoticeInput(clientId: string): boolean { return this.marker('notice-input:' + clientId) > Date.now(); }
  rememberNoticeDelivery(replyId: string): void {
    this.local.prepare('INSERT INTO markers VALUES(?,?) ON CONFLICT(key) DO NOTHING')
      .run('notice-delivered:' + replyId, canonical(Date.now()));
  }
  private marker(key: string): number { const row = this.local.prepare('SELECT value_json FROM markers WHERE key=?').get(key); return row ? Number(JSON.parse(String(row['value_json']))) : -1; }
  private protectedPins(): Set<string> {
    const result=new Set<string>();
    for(const row of this.local.prepare("SELECT d.id,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/main'").all()){
      const main=JSON.parse(String(row['value_json'])) as Chat.Main,pending=main.pendingAction;if(!pending)continue;
      result.add(pending.objectId+':'+pending.revision);
      const operation=this.local.prepare("SELECT r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.id=? AND d.key='chat-bridge/operation'").get(pending.objectId);
      if(operation){const value=JSON.parse(String(operation['value_json'])) as Chat.Operation;if(value.request.action==='createMain'){
        if(value.request.expectedMainRevision!==null)result.add(String(row['id'])+':'+value.request.expectedMainRevision);
        if(value.request.resumeMain)result.add(value.request.resumeMain.objectId+':'+value.request.resumeMain.revision);
      }}
    }
    return result;
  }
  private technicalTree(rootId:string,includeRoot:boolean):string[] {
    const selected=new Set<string>(includeRoot?[rootId]:[]),queue=[rootId];
    while(queue.length){const parent=queue.shift()!;for(const row of this.local.prepare('SELECT id FROM documents WHERE parent_id=?').all(parent)){const id=String(row['id']);if(!selected.has(id)){selected.add(id);queue.push(id);}}}
    return [...selected];
  }
  private deleteTechnicalTree(rootId:string,includeRoot:boolean):void {
    const ids=this.technicalTree(rootId,includeRoot),parents=[rootId,...ids];
    for(const parent of parents)this.local.prepare('DELETE FROM blobs WHERE parent_id=?').run(parent);
    for(const id of ids.reverse()){this.local.prepare('DELETE FROM revisions WHERE id=?').run(id);this.local.prepare('DELETE FROM documents WHERE id=?').run(id);}
  }
  private pruneHistoricalRevisions(protectedPins=this.protectedPins(),id?:string):void {
    for(const row of this.local.prepare('SELECT r.id,r.revision,d.key,d.current_revision FROM revisions r JOIN documents d ON d.id=r.id WHERE r.revision<>d.current_revision AND (? IS NULL OR r.id=?)').all(id??null,id??null))
      if(!(row['key']==='chat-bridge/collection'&&Number(row['revision'])>Number(row['current_revision'])-32)&&!protectedPins.has(String(row['id'])+':'+Number(row['revision'])))
        this.local.prepare('DELETE FROM revisions WHERE id=? AND revision=?').run(String(row['id']),Number(row['revision']));
  }
  private expireOperations(now = Date.now(),force=false): void {
    if(!force&&now-this.lastMaintenance<60_000)return;
    const protectedPins=this.protectedPins(),protectedObjects=new Set([...protectedPins].map(value=>value.slice(0,value.lastIndexOf(':'))));
    const queuedInputs = new Set<string>();
    const queuedNotices = new Set<string>();
    for (const row of this.local.prepare("SELECT r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/main'").all())
      for (const ticket of (JSON.parse(String(row['value_json'])) as Chat.Main).queue ?? []) {
        queuedInputs.add(ticket.inputId);
        if (ticket.identity) queuedNotices.add(hashJson([ticket.identity.senderPrincipalId, ticket.identity.messageId]));
      }
    const rows = this.local.prepare("SELECT d.id,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/operation'").all();
    const pendingOffers = new Set(rows.flatMap(row => {
      const value = JSON.parse(String(row['value_json'])) as Chat.Operation;
      return !['succeeded', 'failed'].includes(value.phase) && value.request?.action === 'acknowledge' ? [value.request.offerId] : [];
    }));
    this.local.exec('BEGIN IMMEDIATE');
    try {
      for (const row of rows) {
        const value = JSON.parse(String(row['value_json'])) as Chat.Operation;
        if (!['succeeded', 'failed'].includes(value.phase) || protectedObjects.has(String(row['id'])) || Date.parse(value.updatedAt) + 86_400_000 >= now) continue;
        // Admission can be complete while its input still waits on the native owner.
        if (value.outcome && 'input' in value.outcome && queuedInputs.has(value.outcome.input.object.objectId)) continue;
        if (value.request?.action === 'notify' && queuedNotices.has(hashJson([value.callerPrincipalId, value.operationId]))) continue;
        // A completed Main turn is not completed transport delivery. Keep the producer receipt
        // until its exact first reply has a confirmed WhatsApp acknowledgement.
        if (value.outcome?.action === 'notify' && this.marker('notice-delivered:' + value.outcome.reply.object.objectId) < 0) continue;
        const offer = this.local.prepare("SELECT id FROM documents WHERE key='chat-bridge/offer' AND name=? AND parent_id IS (SELECT parent_id FROM documents WHERE id=?)")
          .get('Chat offer ' + String(row['id']), String(row['id']));
        // An accepted acknowledgement must still resolve its original offer after a restart.
        if (offer && pendingOffers.has(String(offer['id']))) continue;
        // Offers and acknowledgements share the workspace root with their operation.
        // They are not descendants and must expire with the receipt that owns them.
        for (const [key, name] of [['chat-bridge/offer', 'Chat offer '], ['chat-bridge/acknowledgement', 'Chat acknowledgement ']]) {
          const receipt = this.local.prepare('SELECT id FROM documents WHERE key=? AND name=? AND parent_id IS (SELECT parent_id FROM documents WHERE id=?)').get(key!, name! + String(row['id']), String(row['id']));
          if (receipt) this.deleteTechnicalTree(String(receipt['id']), true);
        }
        if (value.outcome?.action === 'notify') this.local.prepare('DELETE FROM markers WHERE key=?').run('notice-delivered:' + value.outcome.reply.object.objectId);
        this.deleteTechnicalTree(String(row['id']),true);
      }
      for(const row of this.local.prepare('SELECT root_id FROM workflow_cleanup WHERE delete_after<=?').all(now)){
        const root=String(row['root_id']),ids=this.technicalTree(root,false);
        if(queuedInputs.has(root)||ids.some(id=>protectedObjects.has(id)))continue;
        this.deleteTechnicalTree(root,false);this.local.prepare('DELETE FROM workflow_cleanup WHERE root_id=?').run(root);
      }
      this.local.prepare("DELETE FROM markers WHERE key LIKE 'notice-input:%' AND CAST(value_json AS INTEGER)<=?").run(now);
      this.local.prepare('INSERT INTO markers VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json').run('operations.expiredBefore', canonical(Math.max(this.marker('operations.expiredBefore'),now-86_400_000)));
      this.pruneHistoricalRevisions(protectedPins);this.local.exec('COMMIT');this.lastMaintenance=now;
    }catch(error){this.local.exec('ROLLBACK');throw error;}
  }
  async validateOperationId(value: string, now = Date.now()): Promise<void> {
    if (!this.runtimeEpoch) this.setRuntimeEpoch((await this.client.request('system.status', {})).runtimeEpoch);
    this.expireOperations(now);
    const identity = parseOperationId(value);
    requireThat(identity.runtimeEpoch === this.runtimeEpoch && identity.issuedAtUnixMs > this.marker('operations.expiredBefore') &&
      identity.issuedAtUnixMs <= now + 60_000 && identity.issuedAtUnixMs + 86_400_000 >= now,
    'operation_expired', 'The ChatBridge operation belongs to another or expired runtime window.');
  }
  private localDocument<K extends ContractKey>(key: K, reference: string | Chat.ObjectPin, parentId: string | null): Document<K> {
    const id = typeof reference === 'string' ? reference : reference.objectId, head = this.local.prepare('SELECT * FROM documents WHERE id=? AND key=?').get(id, key);
    requireThat(head && (head['parent_id'] ?? null) === parentId, 'chat_scope_mismatch', 'The selected local ChatBridge record is outside the active scope.');
    const requested = typeof reference === 'string' ? Number(head!['current_revision']) : reference.revision;
    let row = this.local.prepare('SELECT * FROM revisions WHERE id=? AND revision=?').get(id, requested);
    if(!row&&requested<Number(head!['current_revision']))row=this.local.prepare('SELECT * FROM revisions WHERE id=? AND revision=?').get(id,Number(head!['current_revision']));
    requireThat(row, 'chat_evidence_mismatch', 'The selected local ChatBridge revision is unavailable.');
    const revision=Number(row!['revision']);
    const value = JSON.parse(String(row!['value_json'])) as ContractValues[K], bytes = Buffer.byteLength(String(row!['value_json']));
    const metadata: Operation.ObjectMetadata = { id, parentId, ownerObjectId: null, name: String(head!['name']), path: '', position: 0, icon: null, contractKey: key,
      currentRevision: Number(head!['current_revision']), contractVersion: version, archivedAt: null, effectivelyArchived: false,
      createdAt: String(head!['created_at']), updatedAt: String(head!['updated_at']) };
    const revisionMetadata: Operation.RevisionMetadata = { objectId: id, revision, contractVersion: version, contentHash: hashJson(value), mediaType: 'application/json',
      byteLength: bytes, createdAt: String(row!['created_at']), references: {} };
    return { key, pin: { objectId: id, revision }, value, metadata, revision: revisionMetadata };
  }
  async read<K extends ContractKey>(key: K, reference: string | Chat.ObjectPin, parentId = this.rootObjectId): Promise<Document<K>> {
    if (localContractKeys.has(key)) return this.localDocument(key, reference, parentId);
    const result = await this.client.request('objects.read', typeof reference === 'string' ? { objectId: reference } : { objectId: reference.objectId, revision: reference.revision });
    requireThat(result.object.contractKey === key && readableVersions.includes(result.revision.contractVersion) && result.revision.mediaType === 'application/json' && result.content.encoding === 'json',
      'chat_contract_mismatch', 'The selected Object is not an exact supported ChatBridge record.');
    requireThat(result.object.parentId === parentId && !result.object.effectivelyArchived, 'chat_scope_mismatch', 'The selected ChatBridge record is outside the active configured scope.');
    requireThat(typeof reference === 'string' || result.revision.revision === reference.revision, 'chat_evidence_mismatch', 'The selected revision differs from its immutable pin.');
    return { key, pin: pinOf(result), value: result.content.value as ContractValues[K], metadata: result.object, revision: result.revision };
  }
  async named<K extends ContractKey>(key: K, name: string, parentId = this.rootObjectId): Promise<Document<K> | null> {
    if (localContractKeys.has(key)) {
      const row = parentId === null ? this.local.prepare('SELECT id FROM documents WHERE key=? AND name=? AND parent_id IS NULL').get(key, name)
        : this.local.prepare('SELECT id FROM documents WHERE key=? AND name=? AND parent_id=?').get(key, name, parentId);
      return row ? this.localDocument(key, String(row['id']), parentId) : null;
    }
    const page = await this.client.request('objects.query', { contractKey: key, includeArchived: true,
      where: { op: 'and', args: [parentId === null ? { op: 'isNull', field: 'object.parentId' } : { op: 'eq', field: 'object.parentId', value: parentId }, { op: 'eq', field: 'object.name', value: name }] }, limit: 2 });
    requireThat(page.items.length <= 1 && !page.nextCursor, 'chat_identity_conflict', 'A ChatBridge durable name must resolve to one record across all versions.');
    return page.items[0] ? this.read(key, page.items[0].objectId, parentId) : null;
  }
  async write<K extends ContractKey>(key: K, value: ContractValues[K], mutationId: string, destination: Destination): Promise<Chat.ObjectPin> {
    if (localContractKeys.has(key)) {
      const encoded = canonical(value), now = new Date().toISOString();
      this.expireOperations();
      const usage = this.local.prepare('SELECT COUNT(*) AS rows,COALESCE(SUM(length(CAST(value_json AS BLOB))),0) AS bytes FROM revisions').get() as { rows: number; bytes: number };
      requireThat(usage.rows < 10_000 && usage.bytes + Buffer.byteLength(encoded) <= 256 * 1024 * 1024, 'capacity_exceeded', 'The ChatBridge workflow journal is full.');
      if ('create' in destination) {
        const existing = await this.named(key, destination.create.name, destination.create.parentId);
        if (existing) { requireThat(existing.pin.revision === 1 && hashJson(existing.value) === hashJson(value), 'revision_conflict', 'The local ChatBridge identity differs.'); return existing.pin; }
        const id = randomUUID(); this.local.exec('BEGIN IMMEDIATE');
        try {
          this.local.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(id, key, destination.create.name, destination.create.parentId, 1, now, now);
          this.local.prepare('INSERT INTO revisions VALUES(?,?,?,?)').run(id, 1, encoded, now); this.local.exec('COMMIT');
        } catch (error) { this.local.exec('ROLLBACK'); const winner = await this.named(key, destination.create.name, destination.create.parentId); if (winner) { if (hashJson(winner.value) === hashJson(value)) return winner.pin; throw new IvyError('revision_conflict', 'The local ChatBridge identity already exists.'); } throw error; }
        this.changeVersion++; return { objectId: id, revision: 1 };
      }
      const next = destination.expectedRevision + 1; this.local.exec('BEGIN IMMEDIATE');
      try {
        const changed = this.local.prepare('UPDATE documents SET current_revision=?,updated_at=? WHERE id=? AND key=? AND current_revision=?').run(next, now, destination.objectId, key, destination.expectedRevision);
        requireThat(changed.changes === 1, 'revision_conflict', 'The local ChatBridge row changed.');
        this.local.prepare('INSERT INTO revisions VALUES(?,?,?,?)').run(destination.objectId, next, encoded, now); this.pruneHistoricalRevisions(undefined,destination.objectId); this.local.exec('COMMIT');
      } catch (error) { this.local.exec('ROLLBACK'); throw error; }
      this.changeVersion++; return { objectId: destination.objectId, revision: next };
    }
    const content: Wire.Content = { encoding: 'json', value: value as Wire.Json };
    const references: Wire.RevisionReferences = {};
    if (key === 'chat-bridge/input') {
      const input = value as Chat.Input; if (input.result) references['result'] = input.result;
      for (const [index, image] of input.payload.images.entries()) references['image-' + index] = image.object;
    } else if (key === 'chat-bridge/result') {
      for (const [index, part] of (value as Chat.Result).textParts.entries()) references['text-' + index] = part.object;
    } else if (key === 'chat-bridge/reply') {
      const reply = value as Chat.Reply; if (reply.origin.kind === 'native') references['result'] = reply.origin.result;
      for (const [index, artifact] of reply.artifacts.entries()) references['artifact-' + index] = artifact.object;
    }
    mutationId = await scopedOperationId(this.client, ['chat', mutationId]);
    let result: Operation.ObjectWriteResult;
    try {
      result = await this.client.request('objects.write', 'create' in destination
        ? { mutationId, contractVersion: version, content, references, create: { ...destination.create, ownerObjectId: null, contractKey: key } }
        : { mutationId, contractVersion: version, content, references, objectId: destination.objectId, expectedRevision: destination.expectedRevision });
    } catch (error) {
      if (!('create' in destination) || !(error instanceof IvyError) || error.code !== 'revision_conflict') throw error;
      const existing = await this.named(key, destination.create.name, destination.create.parentId);
      requireThat(existing && existing.pin.revision === 1 && hashJson(existing.value) === hashJson(value), 'revision_conflict', 'The durable ChatBridge identity already contains different content.');
      return existing.pin;
    }
    if(key==='chat-bridge/input'){
      const input=value as Chat.Input;
      if(['completed','failed','cancelled'].includes(input.state)&&input.finishedAt){
        this.local.prepare('INSERT INTO workflow_cleanup VALUES(?,?) ON CONFLICT(root_id) DO NOTHING').run(result.object.id,Date.parse(input.finishedAt)+86_400_000);
        this.expireOperations(Date.now(),true);
      }
    }
    this.changeVersion++; return pinOf(result);
  }
  localBlob(key: string, pin: Chat.ObjectPin, parentId: string): Buffer {
    requireThat(pin.revision === 1, 'chat_evidence_mismatch', 'Local evidence blobs have one immutable revision.');
    const row = this.local.prepare('SELECT bytes FROM blobs WHERE id=? AND key=? AND parent_id=?').get(pin.objectId, key, parentId);
    requireThat(row, 'chat_evidence_mismatch', 'The local evidence blob is unavailable.'); return Buffer.from(row!['bytes'] as Uint8Array);
  }
  localInfo(objectId: string): { key: string; parentId: string | null } | null {
    const row = this.local.prepare('SELECT key,parent_id FROM documents WHERE id=?').get(objectId) ?? this.local.prepare('SELECT key,parent_id FROM blobs WHERE id=?').get(objectId);
    return row ? { key: String(row['key']), parentId: row['parent_id'] === null ? null : String(row['parent_id']) } : null;
  }
  async requireAvailable(reference: Chat.ObjectPin): Promise<void> {
    const local = this.localInfo(reference.objectId);
    if (local && localContractKeys.has(local.key as ContractKey)) {
      this.localDocument(local.key as ContractKey, reference, local.parentId);
      return;
    }
    const value = await this.client.request('objects.read', reference);
    requireThat(!value.object.effectivelyArchived && value.revision.revision === reference.revision,
      'chat_notice_source_unavailable', 'A notice needs an existing unarchived source revision.');
  }
  saveLocalBlob(key: string, name: string, parentId: string, bytes: Buffer): Chat.ObjectPin {
    this.expireOperations();
    const existing = this.local.prepare('SELECT id,bytes FROM blobs WHERE key=? AND name=? AND parent_id=?').get(key, name, parentId);
    if (existing) { requireThat(Buffer.from(existing['bytes'] as Uint8Array).equals(bytes), 'revision_conflict', 'The local evidence identity differs.'); return { objectId: String(existing['id']), revision: 1 }; }
    const usage = this.local.prepare('SELECT COUNT(*) AS rows,COALESCE(SUM(length(bytes)),0) AS bytes FROM blobs').get() as { rows: number; bytes: number };
    requireThat(usage.rows < 10_000 && usage.bytes + bytes.length <= 256 * 1024 * 1024, 'capacity_exceeded', 'The ChatBridge workflow journal is full.');
    const id = randomUUID(); this.local.prepare('INSERT INTO blobs VALUES(?,?,?,?,?)').run(id, key, name, parentId, bytes); return { objectId: id, revision: 1 };
  }
  close(): void { this.local.close(); }
}
