import { BufferJSON, initAuthCreds, proto } from 'baileys';
import type { AuthenticationState, AuthenticationCreds, SignalDataTypeMap } from 'baileys';
import { DatabaseSync } from 'node:sqlite';
import { mkdirSync } from 'node:fs';
import { dirname } from 'node:path';
export const encode=(value: unknown):string=>JSON.stringify(value,BufferJSON.replacer);
export const decode=<T>(value: string):T=>JSON.parse(value,BufferJSON.reviver) as T;

export class WhatsAppAuthStore {
  readonly db:DatabaseSync;
  constructor(path:string){
    if(path!==':memory:')mkdirSync(dirname(path),{recursive:true,mode:0o700});
    this.db=new DatabaseSync(path);this.db.exec(`PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;
      CREATE TABLE IF NOT EXISTS auth(category TEXT NOT NULL,id TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(category,id));`);
  }
  get(category:string,id:string):string|null {const row=this.db.prepare('SELECT value FROM auth WHERE category=? AND id=?').get(category,id);return row?String(row['value']):null;}
  set(category:string,id:string,value:string|null):void {
    if(this.get(category,id)===value)return;
    if(value===null)this.db.prepare('DELETE FROM auth WHERE category=? AND id=?').run(category,id);
    else this.db.prepare('INSERT INTO auth VALUES(?,?,?) ON CONFLICT(category,id) DO UPDATE SET value=excluded.value').run(category,id,value);
  }
  transaction(fn:()=>void):void {this.db.exec('BEGIN IMMEDIATE');try{fn();this.db.exec('COMMIT');}catch(error){this.db.exec('ROLLBACK');throw error;}}
  close():void {this.db.close();}
}

export function whatsappAuth(store: WhatsAppAuthStore): { state: AuthenticationState; save: ()=>void } {
  const saved=store.get('creds','default'), creds=saved?decode<AuthenticationCreds>(saved):initAuthCreds();
  const state: AuthenticationState={creds,keys:{
    get: async <T extends keyof SignalDataTypeMap>(type:T,ids:string[])=>{
      const result: {[id:string]:SignalDataTypeMap[T]}={};
      for(const id of ids){const raw=store.get(type,id); if(raw){let value=decode<SignalDataTypeMap[T]>(raw); if(type==='app-state-sync-key') value=proto.Message.AppStateSyncKeyData.fromObject(value as object) as unknown as SignalDataTypeMap[T]; result[id]=value;}}
      return result;
    },
    set: async data=>store.transaction(()=>{for(const [category,values] of Object.entries(data)) for(const [id,value] of Object.entries(values??{})) store.set(category,id,value==null?null:encode(value));}),
  }};
  const save=()=>store.set('creds','default',encode(creds));save();return {state,save};
}
