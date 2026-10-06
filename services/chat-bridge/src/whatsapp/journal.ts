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
  constructor(path: string) {
    mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
    const disk = new DatabaseSync(path), schema=`
      CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS inbox (id TEXT PRIMARY KEY,sender TEXT NOT NULL,jid TEXT NOT NULL,message TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending',request TEXT,received INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS outgoing (sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,jid TEXT NOT NULL,text TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'pending');
      CREATE INDEX IF NOT EXISTS inbox_pending ON inbox(received,id) WHERE state='pending';
      CREATE INDEX IF NOT EXISTS outgoing_pending ON outgoing(jid,sequence) WHERE state<>'sent';`;
    disk.exec('PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;'+schema);
    this.db=new DatabaseSync(':memory:');this.db.exec(schema);
    this.db.exec('BEGIN');
    for(const table of ['kv','inbox','outgoing'])for(const row of disk.prepare('SELECT * FROM '+table).all()){
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
    const prior = this.db.prepare('SELECT sender,jid,message FROM inbox WHERE id=?').get(row.id);
    if (prior) { requireThat(prior['sender'] === row.sender && prior['jid'] === row.jid && prior['message'] === row.message, 'whatsui_identity_conflict', 'The original WhatsApp message changed.'); return; }
    this.write('INSERT INTO inbox(id,sender,jid,message,received) VALUES(?,?,?,?,?)',row.id,row.sender,row.jid,row.message,row.received);
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
  enqueue(key: string, jid: string, text: string): string[] {
    const chars = [...text], ids: string[] = [];
    for (let offset=0;offset<chars.length;offset+=3500) {
      const part=chars.slice(offset,offset+3500).join(''), id=identity('out',key,offset).slice(0,32).toUpperCase(); ids.push(id);
      const previous=this.db.prepare('SELECT jid,text FROM outgoing WHERE id=?').get(id);
      requireThat(!previous || previous['jid']===jid && previous['text']===part,'whatsui_identity_conflict','A saved reply cannot change its recipient or text.');
      if(!previous){this.write('INSERT OR IGNORE INTO outgoing(id,jid,text) VALUES(?,?,?)',id,jid,part);this.set('out-created:'+id,Date.now());}
    }
    return ids;
  }
  hasOutput(key: string): boolean {
    return !!this.db.prepare('SELECT 1 FROM outgoing WHERE id=?').get(identity('out', key, 0).slice(0,32).toUpperCase());
  }
  /** Retain the original wording of a single-part notice across service updates. */
  noticeText(key: string): string | null {
    return this.db.prepare('SELECT text FROM outgoing WHERE id=?').get(identity('out', key, 0).slice(0,32).toUpperCase())?.['text'] as string ?? null;
  }
  outgoing(): Outgoing[] { return this.db.prepare("SELECT * FROM outgoing WHERE state<>'sent' ORDER BY sequence LIMIT 16").all() as unknown as Outgoing[]; }
  mark(id: string, state: 'sending'|'sent'|'unknown'): void { this.write("UPDATE outgoing SET state=? WHERE id=? AND state<>'sent'",state,id); }
  sent(ids: string[]): boolean { return ids.every(id=>this.db.prepare("SELECT 1 FROM outgoing WHERE id=? AND state='sent'").get(id)); }
  prune(now=Date.now()):void {
    const cutoff=now-7*86400_000;
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
      for(const row of this.db.prepare("SELECT id FROM inbox WHERE state='done' AND received<? LIMIT 256").all(cutoff)){
        const id=String(row['id']);this.finish(id);this.set('read-receipt:'+id,null);this.set('local-input:'+id,null);this.set('inbox-content:'+id,null);
        this.write('DELETE FROM inbox WHERE id=?',id);
      }
      const messages:string[]=[],outputs:string[]=[];
      for(const row of this.db.prepare("SELECT key FROM kv WHERE key LIKE 'message:%' AND json_extract(value,'$.at')<?").iterate(cutoff)){
        const key=String(row['key']);if(protectedMessages.some(prefix=>key.startsWith(prefix)))continue;
        messages.push(key);if(messages.length===256)break;
      }
      for(const key of messages)this.set(key,null);
      for(const row of this.db.prepare("SELECT o.id FROM outgoing o JOIN kv k ON k.key='out-created:'||o.id WHERE o.state='sent' AND CAST(k.value AS INTEGER)<?").iterate(cutoff)){
        const id=String(row['id']);if(protectedOutputs.has(id))continue;
        outputs.push(id);if(outputs.length===256)break;
      }
      for(const id of outputs){
        this.write('DELETE FROM outgoing WHERE id=?',id);this.set('out-created:'+id,null);this.set('send-attempt:'+id,null);
      }
    });
  }
  close():Promise<void> {return this.closing??=(async()=>{try{await this.flush();}finally{await this.writer?.terminate();this.db.close();}})();}
}
