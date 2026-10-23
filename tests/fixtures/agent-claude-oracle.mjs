import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {join} from 'node:path';
import {DatabaseSync} from 'node:sqlite';
import {reference,sourceIdentity} from './agent-claude-reference.mjs';

const input=JSON.parse(readFileSync(0,'utf8'));
const original=reference(input.root,input.clock);
const path=join(input.root,'state.sqlite');
if(input.seedSql){const seed=new DatabaseSync(path);seed.exec(input.seedSql);seed.close();}
let store=new original.SessionStore(path);
const outputs=[];
const allowed=new Set(['upsertThread','getThread','listThreads','listSections','getSection','createSection','updateSection','deleteSection','moveThreadToSection','updateThreadStatus','updateThreadName','updateClaudeSessionId','updateCodexSessionId','setArchived','upsertTurn','getTurn','listTurns','deleteRecentTurns','appendItem','updateItem','updateItemAndMoveToEnd','completeTurn','updateTurnDiff','recoverStaleInProgressTurns']);
for(const op of input.ops){
 try{
  let value;
  if(op.method==='clock'){original.setClock(op.args[0]);value=null;}
  else if(op.method==='reopen'){store.close();store=new original.SessionStore(path);value=null;}
  else if(op.method==='sql'){
   assert(!/attach|load_extension|vacuum|pragma\s+(?:temp_store_directory|data_store_directory)/i.test(op.args[0]));
   store.db.exec(op.args[0]);value=null;
  }else{
   assert(allowed.has(op.method));
   const args=structuredClone(op.args);
   if(op.method==='updateItem'||op.method==='updateItemAndMoveToEnd'){
    const update=args[2];args[2]=item=>({...item,...update});
   }
   value=store[op.method](...args)??null;
  }
  outputs.push({value});
 }catch(error){outputs.push({error:{name:error.name,message:error.message,code:error.code??null}});}
}
store.close();
const db=new DatabaseSync(path);
const raw={};
for(const table of ['threads','turns','thread_sections']){
 const statement=db.prepare('SELECT rowid,* FROM '+table+' ORDER BY id');statement.setReadBigInts(true);
 raw[table]=statement.all().map(row=>Object.fromEntries(Object.entries(row).map(([key,value])=>[key,typeof value==='bigint'?(value>=BigInt(Number.MIN_SAFE_INTEGER)&&value<=BigInt(Number.MAX_SAFE_INTEGER)?Number(value):{integer:String(value)}):value])));
}
raw.schema=db.prepare("SELECT type,name,tbl_name,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type,name").all();
const sqliteVersion=db.prepare('SELECT sqlite_version() version').get().version;
db.close();
console.log(JSON.stringify({sourceIdentity,sqliteVersion,outputs,raw}));
