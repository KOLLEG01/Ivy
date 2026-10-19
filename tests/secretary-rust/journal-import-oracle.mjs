import assert from 'node:assert/strict';
import {mkdtempSync,mkdirSync,readFileSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {DatabaseSync} from 'node:sqlite';
import {load,rootId,itemId} from './inbox-fixtures.mjs';
import {snapshot,seedRetainedValues,seedTechnicalHistory} from './import-core-fixtures.mjs';
import {importContextLink} from './follow-up-import-reference.mjs';
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {IvyError,canonical}=await load('packages/sdk/src/node.js');
const key='secretary/follow-up',contexts='secretary/follow-up-context';
const request={operationId:'fixture:1800000000000:question',itemId,question:'retained \ud800 question'};
const question={request,callerPrincipalId:'fixture',threadId:null,turnId:null,calls:null,phase:'prepared',answer:null,errorCode:null,expiresAt:1800000000000};
const context={itemId,threadId:'original-thread',target:{serviceNodeId:'original-target',nativeVersion:'original-version'},start:{operationId:'retained-start',params:{value:'\udc00'}},expiresAt:1800000000000};
const directory=mkdtempSync(join(tmpdir(),'secretary-journal-import-reference-')),rows=[];
let activeStore;
const doc=row=>({pin:{objectId:row.id,revision:row.current_revision},name:row.name,value:JSON.parse(row.value_json)});
function importAcks(store,after,limit){
 if(!Number.isInteger(limit)||limit<1||limit>1000)throw new IvyError('invalid_request','Select an import page of 1 to 1000 checkpoints.');
 store.db.exec('BEGIN IMMEDIATE');
 try{
  const selected=store.db.prepare("SELECT id,name,current_revision,value_json FROM documents WHERE key='secretary/operation' AND json_extract(value_json,'$.action')='checkpoint' AND json_extract(value_json,'$.phase')='succeeded' AND (? IS NULL OR id>?) ORDER BY id LIMIT ?").all(after,after,limit+1);
  const sources=selected.slice(0,limit).map(doc),inserted=[];
  for(const source of sources){
   const captures=source.value.checkpointCaptures;if(captures==null)continue;
   const invalid=()=>new IvyError('secretary_import_checkpoint_invalid','The retained checkpoint has no valid capture identity.');
   if(!Array.isArray(captures))throw invalid();
   for(const capture of captures){
    const id=capture?.objectId;if(typeof id!=='string'||!id.isWellFormed())throw invalid();
    if(store.db.prepare('INSERT OR IGNORE INTO capture_acknowledgements VALUES(?)').run(id).changes)inserted.push(id);
   }
  }
  store.db.exec('COMMIT');return{sources,inserted,nextCursor:selected.length>limit?sources.at(-1).pin.objectId:null};
 }catch(error){store.db.exec('ROLLBACK');if(error instanceof IvyError)throw error;throw new IvyError('storage_unavailable','SQLite refused the explicit import.','unknown');}
}
const link=(name='question')=>({kind:'link',name}),acks=(after=null,limit=2)=>({kind:'acks',after,limit});
function add(label,options={}){
 const location=join(directory,String(rows.length));mkdirSync(location);mkdirSync(join(location,'source'));
 const client={async request(){assert.fail('Local import must not request any RPC.');}};
 let store=new SecretaryStore(client,rootId,join(location,'source'));activeStore=store;seedRetainedValues(store);
 const definitions=options.documents??[{id:'question',key,name:'question',value:question},{id:'request-context',key:contexts,name:'question',value:context},{id:'shared-context',key:contexts,name:itemId,value:{...context,threadId:'shared-thread'}}];
 for(const entry of definitions)seedTechnicalHistory(store,entry.key,entry.name,entry.value,entry.id);
 for(const [index,change] of (options.checkpoints??[]).entries()){
  const value=JSON.parse(JSON.stringify({action:'checkpoint',phase:'succeeded',checkpointCaptures:[{objectId:'capture-'+index,revision:2}],...change}));
  const id='checkpoint-'+index,encoded=canonical(value);
  store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(id,change.key??'secretary/operation',id,2,index,1,encoded);
  store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(id,1,'{ "original":"\\ud800" }');store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(id,2,encoded);
 }
 for(const id of options.acknowledged??[])store.db.prepare('INSERT INTO capture_acknowledgements VALUES(?)').run(id);
 if(options.sqlFailure)store.db.exec("CREATE TRIGGER import_ack_refusal BEFORE INSERT ON capture_acknowledgements WHEN NEW.capture_id='refuse' BEGIN SELECT RAISE(ABORT,'import refused'); END;");
 const before=snapshot(store.db);store.close();activeStore=undefined;
 const sourceBytes=readFileSync(join(location,'source','secretary.sqlite')),path=join(location,'copy.sqlite');writeFileSync(path,sourceBytes);
 const results=[];
 for(const action of options.actions??[link(),link()]){
  // Actual original core bytes are reopened without its constructor's implicit
  // acknowledgement conversion. This is the approved explicit import policy.
  const db=new DatabaseSync(path);db.exec('PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA busy_timeout=5000');
  store=Object.assign(Object.create(SecretaryStore.prototype),{db,client,root:rootId,limits:{count:options.noRoom?0:10000,bytes:256*1024*1024}});
  activeStore=store;
  const opened=snapshot(db);let result;
  try{
   const value=action.kind==='acks'?importAcks(store,action.after,action.limit):importContextLink(store,action.name);
   result={value};
  }catch(error){result={code:IvyError.from(error).code};}
  results.push({opened,result,after:snapshot(db)});store.close();activeStore=undefined;
 }
 assert.ok(sourceBytes.equals(readFileSync(join(location,'source','secretary.sqlite'))));
 rows.push({input:{label,coreBase64:sourceBytes.toString('base64'),actions:options.actions??[link(),link()],noRoom:!!options.noRoom},expected:{before,results}});
}
try{
 add('request-precedes-shared');
 add('shared-only',{documents:[{id:'question',key,name:'question',value:question},{id:'shared-context',key:contexts,name:itemId,value:context}]});
 add('multiple-retained-contexts',{documents:[{id:'question',key,name:'question',value:question},{id:'second-question',key,name:'second',value:{...question,request:{...request,operationId:'other'}}},{id:'first-context',key:contexts,name:'question',value:context},{id:'second-context',key:contexts,name:'second',value:{...context,threadId:'second-thread',target:{serviceNodeId:'second-target'}}},{id:'shared-context',key:contexts,name:itemId,value:{...context,threadId:'shared-thread'}}],actions:[link(),link('second'),link()]});
 add('current-link-wins',{documents:[{id:'question',key,name:'question',value:{...question,contextObjectId:'shared-context'}},{id:'legacy-context',key:contexts,name:'question',value:context},{id:'shared-context',key:contexts,name:itemId,value:{...context,threadId:'shared-thread'}}]});
 add('current-null-wins',{documents:[{id:'question',key,name:'question',value:{...question,contextObjectId:null}},{id:'legacy-context',key:contexts,name:'question',value:context}]});
 add('current-missing-link-remains',{documents:[{id:'question',key,name:'question',value:{...question,contextObjectId:'originally-bound'}}]});
 add('no-context',{documents:[{id:'question',key,name:'question',value:question}]});
 add('no-question',{documents:[],actions:[link()]});
 add('mismatched-item',{documents:[{id:'question',key,name:'question',value:question},{id:'legacy-context',key:contexts,name:'question',value:{...context,itemId:'foreign'}}]});
 add('insufficient-room',{noRoom:true});
 add('expired-original-thread',{documents:[{id:'question',key,name:'question',value:{...question,threadId:'original-thread',phase:'outcome_unknown'}},{id:'legacy-context',key:contexts,name:'question',value:{...context,expiresAt:0}}]});
 add('checkpoint-pages-reopen',{checkpoints:[{},{checkpointCaptures:[{objectId:'capture-0',revision:1},{objectId:'capture-1',revision:9}]},{},{phase:'accepted'},{phase:'failed'},{action:'capture'},{key:'secretary/source'}],actions:[acks(null,2),acks('checkpoint-1',2),acks(null,2)]});
 add('checkpoint-already-acknowledged',{checkpoints:[{}],acknowledged:['capture-0'],actions:[acks(),acks()]});
 add('checkpoint-null-captures',{checkpoints:[{checkpointCaptures:null}],actions:[acks()]});
 add('checkpoint-missing-captures',{checkpoints:[{checkpointCaptures:undefined}],actions:[acks()]});
 add('checkpoint-non-array',{checkpoints:[{checkpointCaptures:{objectId:'invalid'}}],actions:[acks()]});
 add('checkpoint-rollback-later-invalid',{checkpoints:[{},{checkpointCaptures:[{objectId:'valid-before-error'},{}]}],actions:[acks(),acks()]});
 add('checkpoint-surrogate-refused',{checkpoints:[{checkpointCaptures:[{objectId:'\ud800'}]}],actions:[acks()]});
 add('checkpoint-sql-rollback',{checkpoints:[{},{checkpointCaptures:[{objectId:'refuse'}]}],sqlFailure:true,actions:[acks(),acks()]});
 add('checkpoint-empty',{checkpoints:[],actions:[acks()]});
 add('checkpoint-bad-limits',{checkpoints:[{}],actions:[acks(null,0),acks(null,1001)]});
 process.stdout.write(JSON.stringify({rows})+'\n');
}finally{activeStore?.close();rmSync(directory,{recursive:true,force:true});}
