import assert from 'node:assert/strict';
import {mkdtempSync,readFileSync,mkdirSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,rootId,itemId,settings,owner,at} from './inbox-fixtures.mjs';
import {importExecutionDelivery} from './execution-import-reference.mjs';
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {IvyError,hashJson,canonical,parseOperationId}=await load('packages/sdk/src/node.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js');
const schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const seed=JSON.parse(schemaSamples(ajv,[schema]).find(row=>row.valid&&row.reference.endsWith('#/$defs/Execution')).input);
const pin={objectId:itemId,revision:2};
const decision={schemaVersion:1,urgency:'critical',notification:'voice',text:'retained notice',reason:'original'};
const base={...seed,result:JSON.stringify(decision)};delete base.delivery;delete base.voice;
const hash=hashJson(base.result);
const channel={adapter:'whatsapp',accountId:'fixture',channelId:'recipient'};
const handoff={executionId:base.executionId,resultHash:hash,operationId:'retained-main-operation',target:{serviceNodeId:'fixture.chat',channel},request:null,state:'queued',reason:'retained\ud800reason',evidence:null,expiresAt:null};
const voice={executionId:base.executionId,resultHash:hash,source:pin,target:{serviceNodeId:'fixture.phone',recipientId:'recipient',requestDefinitionHash:'sha256:'+'a'.repeat(64)},request:{operationId:'retained-voice-operation',recipientId:'recipient',route:'voice',voicePrompt:'retained prompt\udc00'},callId:'retained-call',state:'outcome_unknown',reason:'retained\ud800reason',expiresAt:null};
const directory=mkdtempSync(join(tmpdir(),'secretary-import-reference-'));
const quote=value=>'"'+value.replaceAll('"','""')+'"';
export function snapshot(db){
 const schema=db.prepare("SELECT type,name,tbl_name AS tableName,sql FROM sqlite_master ORDER BY type,name").all();
 const tables=schema.filter(row=>row.type==='table').map(row=>{
  const columns=db.prepare('PRAGMA table_info('+quote(row.name)+')').all().map(column=>column.name);
  const selected=['rowid',...columns].flatMap((column,i)=>['typeof('+quote(column)+') AS t'+i,'hex(CAST('+quote(column)+' AS BLOB)) AS b'+i]);
  const data=db.prepare('SELECT '+selected.join(',')+' FROM '+quote(row.name)+' ORDER BY rowid').all();
  return{name:row.name,columns,rows:data.map(value=>['rowid',...columns].flatMap((_,i)=>[value['t'+i],value['b'+i]]))};
 });
 return{schema,tables};
}
const rows=[];
async function add(label,options={}){
 const location=join(directory,String(rows.length));mkdirSync(location);
 const original=join(location,'original'),copy=join(location,'copy');mkdirSync(original);mkdirSync(copy);
 const key='secretary/execution',trace=[],steps=[],counts={},history=[{revision:1,value:{...structuredClone(base),phase:'queued'}},{revision:2,value:{...structuredClone(base),...structuredClone(options.current??{})}}];
 let store;
 const read=revision=>{
  const selected=revision?history.find(entry=>entry.revision===revision):history.at(-1);assert(selected);
  const value=selected.value;
  return{object:{id:itemId,parentId:rootId,effectivelyArchived:false,contractKey:key},revision:{revision:selected.revision,contractVersion:contractVersion(key),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:store.objectWriteRequest(key,value,{object:pin},'fixture:1800000000000:import').references,createdAt:at},content:{encoding:'json',value:structuredClone(value)}};
 };
 const client={async request(method,params,requestOptions){
  const normalized=structuredClone(params);if(method==='objects.write'){parseOperationId(normalized.mutationId);normalized.mutationId='<scoped-operation>';}
  trace.push({method,params:normalized,timeout:requestOptions?.timeoutMs??35000});counts[method]=(counts[method]??0)+1;
  let value,error;
  if(method==='serviceNodes.get')value=options.ownerChanged?{...owner,principalId:'foreign'}:owner;
  else if(method==='system.status')value={runtimeEpoch:'fixture'};
  else if(method==='objects.read'){
   if(options.readFailure)error=new IvyError('fixture_failure','Fixture original read failed.','unknown');else value=read(params.revision);
  }else if(method==='objects.write'){
   assert.equal(params.expectedRevision,history.at(-1).revision);
   const first=counts[method]===1,mode=options.writeFailure;
   if(!first||mode!=='unknown-not-applied')history.push({revision:history.at(-1).revision+1,value:structuredClone(params.content.value)});
   if(first&&mode)error=new IvyError(mode==='conflict-applied'?'revision_conflict':'fixture_failure','Fixture original write failed.',mode==='conflict-applied'?'not_executed':'unknown');
   else value={object:{id:itemId},revision:{revision:history.at(-1).revision}};
  }else assert.fail('Import must not dispatch '+method);
  if(error){steps.push({method,error:{code:error.code,message:error.message,outcome:error.outcome}});throw error;}
  steps.push({method,value:structuredClone(value)});return structuredClone(value);
 }};
 store=new SecretaryStore(client,rootId,original);
 store.db.exec("PRAGMA user_version=37;CREATE TABLE retained_unknown(text_value TEXT,integer_value INTEGER,real_value REAL,blob_value BLOB);INSERT INTO retained_unknown VALUES('unrelated',42,0.25,x'00ff00d800');");
 store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run('retained-document','retained/unknown','retained',2,100,null,'{ "value": "\\ud800", "revision": 2 }');
 store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run('retained-document',1,'{ "value": "\\udc00", "revision": 1 }');
 store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run('retained-document',2,'{ "value": "\\ud800", "revision": 2 }');
 store.db.prepare('INSERT INTO technical_blobs VALUES(?,?,?,?)').run('retained-blob','retained/bytes','name',Buffer.from([0,255,0,216,0]));
 store.db.prepare('INSERT INTO technical_cleanup VALUES(?,?)').run('unrelated-root',1800000000000);
 store.db.prepare('INSERT INTO journal_state VALUES(?,?)').run('retained-unknown','{ "raw":"\\ud800" }');
 store.db.prepare('INSERT INTO retired_operations VALUES(?,?)').run('retained:1800000000000:operation',1800000000000);
 const legacy={...options.main?{delivery:{...structuredClone(handoff),...options.main}}:{},...options.voice?{voice:{...structuredClone(voice),...options.voice}}:{}};
 for(const [field,value] of Object.entries(legacy)){
  const id='legacy-'+field,record=field==='delivery'?'secretary/assignment-handoff':'secretary/assignment-voice';
  store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(id,record,base.executionId,3,canonical(value));
  for(let revision=1;revision<=3;revision++)store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(id,revision,revision===3?canonical(value):JSON.stringify({retainedHistory:revision,value:'\ud800',operationId:'retained-original-'+revision}));
 }
 const before=snapshot(store.db),sourceVersion=store.db.prepare('PRAGMA user_version').get().user_version;
 store.close();
 const bytes=readFileSync(join(original,'secretary.sqlite'));writeFileSync(join(copy,'secretary.sqlite'),bytes);
 store=new SecretaryStore(client,rootId,copy);
 const execution=store.decode(key,read());
 const controller=new AbortController();if(options.cancel)controller.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings,owner:{serviceNodeId:'fixture.secretary'},signal:controller.signal,now:()=>new Date(at)});
 const results=[];
 for(let attempt=0;attempt<(options.attempts??1);attempt++){
  try{results.push({value:await importExecutionDelivery(engine,execution)});}catch(cause){const e=IvyError.from(cause);results.push({error:{code:e.code,message:e.message,outcome:e.outcome}});}
 }
 const after=snapshot(store.db);assert.deepEqual(after,before,'Explicit import must retain every local table/revision/value/blob');
 assert.equal(store.db.prepare('PRAGMA user_version').get().user_version,sourceVersion);
 rows.push({input:{label,execution,settings,cancel:!!options.cancel,attempts:options.attempts??1,coreBase64:bytes.toString('base64'),steps},expected:{results,trace,core:after,history:structuredClone(history),sourceVersion}});
 store.close();
}
try{
 await add('no-legacy');
 await add('main-admission',{main:{}});
 await add('voice-admission',{voice:{}});
 await add('both-admissions',{main:{},voice:{},attempts:2});
 for(const state of ['confirmed','failed','fallback','dispatching','outcome_unknown'])await add('voice-state-'+state,{voice:{state}});
 await add('current-wins',{main:{resultHash:'sha256:'+'0'.repeat(64)},voice:{executionId:'historical'},current:{delivery:handoff,voice},attempts:2});
 await add('wrong-main-identity',{main:{executionId:'foreign'}});
 await add('wrong-main-result',{main:{resultHash:'sha256:'+'0'.repeat(64)}});
 await add('wrong-voice-source',{voice:{source:{objectId:rootId,revision:2}}});
 await add('both-late-invalid',{main:{},voice:{source:{objectId:itemId,revision:3}}});
 await add('invalid-mapping-schema',{main:{reason:{invalid:true}}});
 await add('owner-changed',{main:{},ownerChanged:true});
 await add('cancelled',{voice:{},cancel:true});
 await add('unavailable-original-read',{main:{},readFailure:true});
 for(const writeFailure of ['unknown-not-applied','unknown-applied','conflict-applied'])await add(writeFailure,{main:{},voice:{},writeFailure,attempts:2});
 process.stdout.write(JSON.stringify({rootId,rows})+'\n');
}finally{rmSync(directory,{recursive:true,force:true});}
