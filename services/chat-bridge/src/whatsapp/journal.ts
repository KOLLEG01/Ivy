import { DatabaseSync } from 'node:sqlite';
import { mkdirSync } from 'node:fs';
import { dirname } from 'node:path';
import { createHash } from 'node:crypto';
import { Worker } from 'node:worker_threads';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';

export const identity = (...parts: unknown[]) => createHash('sha256').update(JSON.stringify(parts)).digest('hex');
export interface Inbox { id: string; sender: string; jid: string; message: string; state: string; request: string | null; received: number }
export interface Outgoing { id: string; jid: string; text: string; state: string; sequence: number }

type Statement = {sql:string;args:Array<string|number|null>};
/** Synchronous memory view, asynchronous durable transport storage. flush() fences only effects
 * that require crash recovery (authentication and WhatsApp acknowledgements). */
export class WhatsAppJournal {
  readonly db: DatabaseSync;
  private writer:Worker|null=null;
  private batches:Statement[][]=[];
  private batch:Statement[]|null=null;
  private scheduled:NodeJS.Immediate|null=null;
  private sequence=0;
  private waiters=new Map<number,{resolve:()=>void;reject:(error:Error)=>void}>();
  private failure:Error|null=null;
  private closing:Promise<void>|null=null;
  private writes:Promise<void>=Promise.resolve();
  constructor(path: string, readonly historyLimits = { count: 10_000, bytes: 64 * 1024 * 1024 }) {
    mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
    const disk = new DatabaseSync(path), schema=`
      CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS inbox (id TEXT PRIMARY KEY,sender TEXT NOT NULL,jid TEXT NOT NULL,message TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending',request TEXT,received INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS outgoing (sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,jid TEXT NOT NULL,text TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending');
      CREATE TABLE IF NOT EXISTS retired_inbox(id TEXT PRIMARY KEY,fingerprint TEXT NOT NULL,retired INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS retired_outgoing(id TEXT PRIMARY KEY,fingerprint TEXT NOT NULL);
      CREATE INDEX IF NOT EXISTS inbox_pending ON inbox(received,id) WHERE state='pending';
      CREATE INDEX IF NOT EXISTS outgoing_pending ON outgoing(jid,sequence) WHERE state<>'sent';`;
    disk.exec('PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;'+schema);
    this.db=new DatabaseSync(':memory:');this.db.exec(schema);
    this.db.exec('BEGIN');
    for(const table of ['kv','inbox','outgoing','retired_inbox','retired_outgoing'])for(const row of disk.prepare('SELECT * FROM '+table).all()){
      const values=Object.values(row) as Array<string|number|null>;
      this.db.prepare('INSERT INTO '+table+' VALUES('+values.map(()=>'?').join(',')+')').run(...values);
    }
    this.db.exec('COMMIT');disk.close();
    if(path!==':memory:'){
      this.writer=new Worker(new URL('./journal-worker.js',import.meta.url),{workerData:path});
      const fail=()=>{this.failure=new IvyError('storage_unavailable','WhatsApp transport storage failed.');for(const pending of this.waiters.values())pending.reject(this.failure);this.waiters.clear();};
      this.writer.on('error',fail);this.writer.on('exit',()=>{if(!this.closing||this.waiters.size)fail();});
      this.writer.on('message',({id,error}:{id:number;error?:boolean})=>{if(error){fail();return;}this.waiters.get(id)?.resolve();this.waiters.delete(id);});
    }
  }
  get<T>(key: string): T | null { const row = this.db.prepare('SELECT value FROM kv WHERE key=?').get(key); return row ? JSON.parse(String(row['value'])) as T : null; }
  private write(sql:string,...args:Array<string|number|null>):void {
    if(this.failure)throw this.failure;requireThat(!this.closing,'storage_unavailable','Transport journal is closed.');
    this.db.prepare(sql).run(...args);
    if(!this.writer)return;
    if(this.batch)this.batch.push({sql,args});else this.batches.push([{sql,args}]);
    this.scheduled??=setImmediate(()=>{this.scheduled=null;void this.flush().catch(()=>{});});
  }
  set(key: string, value: unknown): void {
    const json=JSON.stringify(value);if(this.db.prepare('SELECT value FROM kv WHERE key=?').get(key)?.['value']===json)return;
    if(value===null)this.write('DELETE FROM kv WHERE key=?',key);
    else this.write('INSERT INTO kv VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value',key,json);
  }
  transaction<T>(fn: () => T): T {
    if(this.batch)return fn();this.db.exec('BEGIN');this.batch=[];
    try { const result=fn();this.db.exec('COMMIT');if(this.batch.length)this.batches.push(this.batch);return result; }
    catch(error){this.db.exec('ROLLBACK');throw error;}finally{this.batch=null;}
  }
  flush():Promise<void> {
    if(this.failure)return Promise.reject(this.failure);
    if(this.scheduled){clearImmediate(this.scheduled);this.scheduled=null;}
    if(this.writer&&this.batches.length){
      const id=++this.sequence,batches=this.batches.splice(0);
      this.writes=new Promise<void>((resolve,reject)=>{this.waiters.set(id,{resolve,reject});this.writer!.postMessage({id,batches});});
      void this.writes.catch(()=>{});
    }
    return this.writes;
  }
  accept(row: Omit<Inbox, 'state' | 'request'>): void {
    const retired = this.db.prepare('SELECT fingerprint FROM retired_inbox WHERE id=?').get(row.id);
    const prior = this.db.prepare('SELECT sender,jid,message FROM inbox WHERE id=?').get(row.id);
    if (prior) {
      requireThat(prior['sender'] === row.sender && prior['jid'] === row.jid &&
        (retired ? retired['fingerprint'] === identity(row.sender,row.jid,row.message) : prior['message'] === row.message || prior['message'] === ''),
      'whatsui_identity_conflict', 'The original WhatsApp message changed.'); return;
    }
    if (retired) return; // An evicted completed input must never re-enter the agent queue.
    this.transaction(() => {
      this.write('INSERT INTO inbox(id,sender,jid,message,received) VALUES(?,?,?,?,?)',row.id,row.sender,row.jid,row.message,row.received);
      this.write('INSERT INTO retired_inbox VALUES(?,?,0)',row.id,identity(row.sender,row.jid,row.message));
    });
  }
  pending(): Inbox[] { return this.db.prepare("SELECT * FROM inbox WHERE state='pending' ORDER BY received,id LIMIT 32").all() as unknown as Inbox[]; }
  saveRequest(id: string, request: unknown): void { this.write('UPDATE inbox SET request=? WHERE id=? AND request IS NULL',JSON.stringify(request),id); }
  rebindRequest(id: string, previous: unknown, next: unknown): void {
    const row = this.db.prepare("SELECT request FROM inbox WHERE id=? AND state='pending'").get(id);
    requireThat(row?.['request'] === JSON.stringify(previous), 'whatsui_identity_conflict',
      'Only the original pending input can adopt a recovered Main binding.');
    this.write("UPDATE inbox SET request=? WHERE id=? AND state='pending'", JSON.stringify(next), id);
  }
  finish(id: string): void {
    this.write("UPDATE inbox SET state='done',request=NULL,message=CASE WHEN EXISTS(SELECT 1 FROM kv WHERE key='inbox-content:'||inbox.id) THEN '' ELSE message END WHERE id=?",id);
    for(const key of ['payload:','transcript:','direct-input:','direct-request:','native:','native:input:','interaction:','interaction:input:','steer-binding:','recover-main:'])this.set(key+id,null);
  }
  enqueue(key: string, jid: string, text: string, automatic = false): string[] {
    const chars = [...text], ids: string[] = [];
    for (let offset=0;offset<chars.length;offset+=3500) {
      const part=chars.slice(offset,offset+3500).join(''), id=identity('out',key,offset).slice(0,32).toUpperCase(); ids.push(id);
      const retired = this.db.prepare('SELECT fingerprint FROM retired_outgoing WHERE id=?').get(id);
      if (retired) { requireThat(retired['fingerprint'] === identity(jid,part), 'whatsui_identity_conflict', 'A delivered reply cannot change its recipient or text.'); continue; }
      const previous=this.db.prepare('SELECT jid,text FROM outgoing WHERE id=?').get(id);
      requireThat(!previous || previous['jid']===jid && previous['text']===part,'whatsui_identity_conflict','A saved reply cannot change its recipient or text.');
      if(automatic)this.set('out-automatic:'+id,true);
      if(!previous){this.write('INSERT OR IGNORE INTO outgoing(id,jid,text) VALUES(?,?,?)',id,jid,part);this.set('out-created:'+id,Date.now());}
    }
    return ids;
  }
  hasOutput(key: string): boolean {
    const id = identity('out', key, 0).slice(0,32).toUpperCase();
    return !!this.db.prepare('SELECT 1 FROM outgoing WHERE id=? UNION ALL SELECT 1 FROM retired_outgoing WHERE id=? LIMIT 1').get(id,id);
  }
  enqueueAttachment(id: string, jid: string, caption: string): void {
    requireThat(!this.db.prepare('SELECT 1 FROM outgoing WHERE id=? UNION ALL SELECT 1 FROM retired_outgoing WHERE id=?').get(id, id),
      'whatsui_identity_conflict', 'A file must retain its original transport identity.');
    this.write('INSERT INTO outgoing(id,jid,text) VALUES(?,?,?)', id, jid, caption);
    this.set('out-created:' + id, Date.now());
  }
  retireAttachment(id: string, requestHash: string): void {
    this.write('INSERT OR IGNORE INTO retired_outgoing VALUES(?,?)', id, 'file:' + requestHash);
    this.write("UPDATE outgoing SET text='',state=CASE WHEN state='sent' THEN 'sent' ELSE 'failed' END WHERE id=?", id);
    this.set('out-created:' + id, null);
  }
  /** Retain the original wording of a single-part notice across service updates. */
  noticeText(key: string): string | null {
    const id = identity('out', key, 0).slice(0,32).toUpperCase();
    if (this.db.prepare('SELECT 1 FROM retired_outgoing WHERE id=?').get(id)) return null;
    return this.db.prepare('SELECT text FROM outgoing WHERE id=?').get(id)?.['text'] as string ?? null;
  }
  outgoing(): Outgoing[] { return this.db.prepare("SELECT * FROM outgoing WHERE state<>'sent' ORDER BY sequence LIMIT 16").all() as unknown as Outgoing[]; }
  mark(id: string, state: 'sending'|'sent'|'unknown'|'failed'): void { this.write("UPDATE outgoing SET state=? WHERE id=? AND state<>'sent'",state,id); }
  sent(ids: string[]): boolean { return ids.every(id=>this.db.prepare("SELECT 1 FROM outgoing WHERE id=? AND state='sent' UNION ALL SELECT 1 FROM retired_outgoing WHERE id=? LIMIT 1").get(id,id)); }
  prune(_now=Date.now()):void {
    const protectedMessages:string[]=[],protectedOutputs=new Set<string>();
    for(const row of this.db.prepare("SELECT key,value FROM kv WHERE key LIKE 'listener:%'").all()){
      const cursor=JSON.parse(String(row['value'])) as {lastTurn?:string;head?:string},binding=String(row['key']).slice('listener:'.length);
      for(const turn of [cursor.lastTurn,cursor.head])if(turn)protectedMessages.push(`message:${binding}:${turn}:`);
    }
    for(const row of this.db.prepare("SELECT key,value FROM kv WHERE key LIKE 'message:%'").all())if(protectedMessages.some(prefix=>String(row['key']).startsWith(prefix))){
      for(const id of (JSON.parse(String(row['value'])) as {ids:string[]}).ids)protectedOutputs.add(id);
    }
    this.transaction(()=>{
      // A partly delivered turn still needs all of its output IDs for acknowledgement.
      for(const row of this.db.prepare("SELECT key,value FROM kv WHERE key LIKE 'turn-delivered:%' AND json_type(value)='array'").all()){
        const ids=JSON.parse(String(row['value'])) as string[];
        if(this.sent(ids))this.set(String(row['key']),true);else for(const id of ids)protectedOutputs.add(id);
      }
      const candidates: Array<{ kind: 'inbox'|'outgoing'|'message'; id: string; at: number; bytes: number }> = [];
      const totals = this.db.prepare("SELECT (SELECT COUNT(*) FROM inbox WHERE id NOT IN (SELECT id FROM retired_inbox WHERE retired=1))+(SELECT COUNT(*) FROM outgoing WHERE id NOT IN (SELECT id FROM retired_outgoing))+(SELECT COUNT(*) FROM kv WHERE key LIKE 'message:%') AS count,COALESCE((SELECT SUM(length(CAST(message AS BLOB))) FROM inbox),0)+COALESCE((SELECT SUM(length(CAST(text AS BLOB))) FROM outgoing),0)+COALESCE((SELECT SUM(length(CAST(value AS BLOB))) FROM kv WHERE key LIKE 'message:%'),0) AS bytes").get()!;
      let count = Number(totals['count']), bytes = Number(totals['bytes']);
      if (count <= this.historyLimits.count && bytes <= this.historyLimits.bytes) return;
      for (const row of this.db.prepare("SELECT id,received,length(CAST(message AS BLOB)) AS bytes FROM inbox WHERE state='done' AND id NOT IN (SELECT id FROM retired_inbox WHERE retired=1)").all())
        candidates.push({ kind:'inbox', id:String(row['id']), at:Number(row['received']), bytes:Number(row['bytes']) });
      for (const row of this.db.prepare("SELECT o.id,COALESCE(CAST(k.value AS INTEGER),0) AS at,length(CAST(o.text AS BLOB)) AS bytes FROM outgoing o LEFT JOIN kv k ON k.key='out-created:'||o.id WHERE o.state='sent' AND o.id NOT IN (SELECT id FROM retired_outgoing)").all())
        if (!protectedOutputs.has(String(row['id'])) && !this.get('out-file:' + row['id'])) candidates.push({ kind:'outgoing', id:String(row['id']), at:Number(row['at']), bytes:Number(row['bytes']) });
      for (const row of this.db.prepare("SELECT key,value,length(CAST(value AS BLOB)) AS bytes FROM kv WHERE key LIKE 'message:%'").all()) {
        const key=String(row['key']), value=JSON.parse(String(row['value'])) as { ids:string[]; at:number };
        if (!protectedMessages.some(prefix=>key.startsWith(prefix)) && this.sent(value.ids))
          candidates.push({ kind:'message', id:key, at:value.at, bytes:Number(row['bytes']) });
      }
      for (const candidate of candidates.sort((a,b)=>a.at-b.at)) {
        if (count <= this.historyLimits.count && bytes <= this.historyLimits.bytes) break;
        const id = candidate.id;
        if (candidate.kind === 'inbox') {
          const row=this.db.prepare('SELECT sender,jid,message FROM inbox WHERE id=?').get(id)!;
          this.write('INSERT INTO retired_inbox VALUES(?,?,1) ON CONFLICT(id) DO UPDATE SET retired=1', id, identity(row['sender'],row['jid'],row['message']));
          this.finish(id); this.set('read-receipt:'+id,null); this.set('local-input:'+id,null); this.set('inbox-content:'+id,null);
          // Compact identity rows also prevent an older binary from admitting the
          // same opaque transport ID after an automatic deployment rollback.
          this.write("UPDATE inbox SET message='',request=NULL,received=? WHERE id=?",Number.MAX_SAFE_INTEGER,id);
        } else if (candidate.kind === 'outgoing') {
          const row=this.db.prepare('SELECT jid,text FROM outgoing WHERE id=?').get(id)!;
          this.write('INSERT OR IGNORE INTO retired_outgoing VALUES(?,?)',id,identity(row['jid'],row['text']));
          this.write("UPDATE outgoing SET text='' WHERE id=?",id); this.set('out-created:'+id,null); this.set('send-attempt:'+id,null); this.set('out-automatic:'+id,null);
        } else this.set(id,null);
        count--; bytes-=candidate.bytes;
      }
    });
  }
  close():Promise<void> {return this.closing??=(async()=>{try{await this.flush();}finally{await this.writer?.terminate();this.db.close();}})();}
}
