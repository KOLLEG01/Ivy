import assert from 'node:assert/strict';
import {load,rootId,itemId,sourceId,operationId,at,id,caller,scope,definition,source,message,policy,settings,owner,assessment,item,request,accepted} from './inbox-fixtures.mjs';
const {SecretaryEngine,operationName,sourceName,messageHash,identityHash,noticeText}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {IvyError,canonical,hashJson}=await load('packages/sdk/src/node.js');
const {validate}=await load('services/secretary/src/schema.js');
const itemName='item-'+item.identityHash.slice(7);
const capture={...accepted,prepared:{key:'secretary/item',destination:{createName:itemName},value:item}};
const gap={contractKey:'secretary/media-manifest',pin:null,contentHash:null,name:'attachment',mediaType:'application/octet-stream',errorCode:'gap'};
const attachmentRequest={action:'attach',operationId:id,expectedScope:scope,sourceId:'mail',item:{objectId:itemId,revision:1},media:[gap]};
const attached={...item,media:[gap]};
const attach={...accepted,action:'attach',request:attachmentRequest,requestHash:hashJson(attachmentRequest),captureIdentity:null,prepared:{key:'secretary/item',destination:{object:{objectId:itemId,revision:1}},value:attached}};
const assessRequest={action:'assess',operationId:id,expectedScope:scope,item:{objectId:itemId,revision:1},assessment};
const assessed={...item,decision:{assessment,actor:caller,operationId:id,decidedAt:at,policy},notice:{state:'queued',reason:null,text:noticeText(assessment,message,definition),source:{objectId:itemId,revision:2},target:null,request:null,preparedRevision:null,evidence:null,voice:null,updatedAt:at}};
const assess={...accepted,action:'assess',request:assessRequest,requestHash:hashJson(assessRequest),captureIdentity:null,prepared:{key:'secretary/item',destination:{object:{objectId:itemId,revision:1}},value:assessed}};
const checkpointRequest={action:'checkpoint',operationId:id,expectedScope:scope,sourceId:'mail',previousCursor:null,nextCursor:'next',captures:[]};
const checkpoint={...accepted,action:'checkpoint',request:checkpointRequest,requestHash:hashJson(checkpointRequest),captureIdentity:null,prepared:{key:'secretary/source',destination:{object:{objectId:sourceId,revision:1}},value:{...source,cursor:'next',checkpointOperation:{principalId:caller,operationId:id},updatedAt:at}}};
for(const value of [capture,attach,assess,checkpoint])validate('Accepted',value);
const local=(value,first=value,objectId=operationId)=>({id:objectId,key:'secretary/operation',name:operationName(value.callerPrincipalId,value.operationId),value,first});
const raw=(value,revision=1)=>({object:{id:itemId,parentId:rootId,name:itemName,effectivelyArchived:false,contractKey:'secretary/item'},revision:{revision,contractVersion:'1.3.0',mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:Object.fromEntries(value.media.filter(m=>m.pin).map((m,i)=>['media-'+i,m.pin])),createdAt:at},content:{encoding:'json',value}});
const rows=[];
async function add(label,{operation=capture,first=operation,records=null,sourceValue=source,sourceFirst=sourceValue,config=settings,hive=null,writeFailure=null,failAt=0,failCode='fixture_failure',failOutcome='unknown',repeats=1,method='apply',limit=10,issues=[],cursor=null,cancelled=false,independentIssue=false}={}){
 const locals=[{id:sourceId,key:'secretary/source',name:sourceName('mail'),value:sourceValue,first:sourceFirst},...(records??[local(operation,first)])];
 const input=structuredClone({label,method,locals,settings:{...config,recordsPerTick:limit},repeats,issues,cursor,cancelled,independentIssue}),steps=[],trace=[];
 let engine;
 let current=structuredClone(hive),writeFailed=false;
 const client={async request(method,params,options){
  trace.push({method,params:structuredClone(params),timeout:options?.timeoutMs??35000});
  if(independentIssue&&trace.length===2)engine.issue('independent','background_issue');
  try{
   if(trace.length===failAt)throw new IvyError(failCode,'Fixture failure.',failOutcome);
   let result;
   if(method==='serviceNodes.get')result=owner;
   else if(method==='system.status')result={runtimeEpoch:'fixture-epoch'};
   else if(method==='objects.query')result={items:current?[{objectId:itemId,revision:current.revision.revision}]:[],nextCursor:null};
   else if(method==='objects.read'){
    if(!current||params.objectId!==itemId||(params.revision!==undefined&&params.revision!==current.revision.revision))throw new IvyError('not_found','Fixture object missing.');
    result=current;
   }else if(method==='objects.write'){
    const fail=writeFailure&&!writeFailed;writeFailed=true;
    if(fail&&!writeFailure.afterWrite)throw new IvyError(writeFailure.code,'Fixture write failure.',writeFailure.outcome);
    if(params.objectId&&current?.revision.revision!==params.expectedRevision)throw new IvyError('revision_conflict','Fixture revision changed.');
    if(params.create&&current)throw new IvyError('mutation_conflict','Fixture name exists.');
    current=raw(params.content.value,params.create?1:params.expectedRevision+1);current.revision.references=params.references;
    if(fail)throw new IvyError(writeFailure.code,'Fixture write failure.',writeFailure.outcome);
    result={object:{id:itemId},revision:{revision:current.revision.revision}};
   }else throw Error(`Unexpected RPC ${method}`);
   steps.push({method,value:structuredClone(result)});return structuredClone(result);
  }catch(cause){const e=IvyError.from(cause);steps.push({method,error:{code:e.code,message:e.message,outcome:e.outcome}});throw cause;}
 }};
 const store=new SecretaryStore(client,rootId),abort=new AbortController();if(cancelled)abort.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 for(const row of locals){const revision=hashJson(row.first)===hashJson(row.value)?1:2,encoded=canonical(row.value),complete=row.key==='secretary/operation'&&row.value.phase!=='accepted';store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(row.id,row.key,row.name,revision,0,complete?1:null,encoded);store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(row.id,1,canonical(row.first));if(revision===2)store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(row.id,2,encoded);}
 engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings:structuredClone(input.settings),owner:{serviceNodeId:'fixture.secretary',generation:1},signal:abort.signal,operationCursor:cursor??undefined,recoveryIssues:new Map(issues)});
 const results=[];
 for(let i=0;i<repeats;i++)try{const value=method==='tick'?await engine.tick():await engine.apply({pin:{objectId:operationId,revision:1},value:operation});results.push({value:value??null});}catch(cause){const e=IvyError.from(cause);results.push({code:e.code,message:e.message,outcome:e.outcome});}
 const retained=store.db.prepare('SELECT id,key,current_revision,value_json,completed_at FROM documents ORDER BY id').all().map(row=>({id:row.id,key:row.key,revision:row.current_revision,value:JSON.parse(row.value_json),completed:row.completed_at!==null}));
 const expected={results,trace,retained,cursor:engine.operationCursor??null,issues:[...engine.recoveryIssues]};store.close();input.steps=steps;rows.push({input,expected});
}
await add('new capture',{repeats:2});await add('retained capture',{hive:raw(item)});
await add('ignored capture',{operation:{...accepted,request:null,ignoredReason:'outgoing',captureIdentity:null},repeats:2});
await add('existing effect',{operation:{...accepted,existingEffect:{objectId:itemId,revision:1}},repeats:2});
await add('already succeeded',{operation:{...capture,phase:'succeeded',effect:{objectId:itemId,revision:1}},first:capture});
await add('already failed',{operation:{...capture,phase:'failed',errorCode:'prior_failure'},first:capture});
await add('cancelled',{cancelled:true});
for(const config of [{...settings,sources:[]},{...settings,sources:[{...definition,producerPrincipalIds:['other']}]},{...settings,sources:[{...definition,ownSenderIds:['sender']}]},{...settings,sources:[{...definition,allowedSenderIds:[]}]}]){
 await add('policy changed new effect',{config});await add('policy changed committed effect',{config,hive:raw(item)});
}
const differentMessage={...message,text:'different'};const different={...item,message:differentMessage,messageHash:messageHash(differentMessage)};
await add('conflicting retained message',{hive:raw(different)});
for(const outcome of ['not_executed','unknown','completed'])for(const code of ['fixture_failure','revision_conflict','mutation_conflict','secretary_identity_conflict']){
 await add('write failure no effect',{writeFailure:{code,outcome,afterWrite:false},repeats:2});
 await add('write failure committed effect',{writeFailure:{code,outcome,afterWrite:true},repeats:2});
}
for(const failAt of [1,2,3,4,5])await add('RPC failure boundary',{failAt,repeats:2});
for(const operation of [attach,assess]){
 await add('amend existing item',{operation,hive:raw(item),repeats:2});
 await add('recover existing amended item',{operation,hive:raw(operation.prepared.value,2)});
 await add('changed revision',{operation,hive:raw(different,2)});
 await add('missing source producer',{operation,hive:raw(item),config:{...settings,sources:[{...definition,producerPrincipalIds:['other']}]}});
 await add('unknown amended write',{operation,hive:raw(item),writeFailure:{code:'fixture_failure',outcome:'unknown',afterWrite:true},repeats:2});
}
await add('checkpoint',{operation:checkpoint,repeats:2});
await add('recover checkpoint',{operation:checkpoint,sourceValue:checkpoint.prepared.value,sourceFirst:source});
await add('checkpoint conflict',{operation:checkpoint,sourceValue:{...source,cursor:'other'},sourceFirst:source});
await add('poison original evidence',{operation:{...capture,requestHash:'sha256:'+'0'.repeat(64)}});
const validIgnored={...accepted,request:null,ignoredReason:'outgoing',captureIdentity:null};
const scanRecords=Array.from({length:34},(_,i)=>{const value={...validIgnored,operationId:`fixture-epoch:1800000000000:scan-${i}`,requestHash:i===33?accepted.requestHash:'sha256:'+'0'.repeat(64)};if(i<33)value.ignoredReason=null;return local(value,value,`00000000-0000-4000-8000-${String(100+i).padStart(12,'0')}`);});
await add('bounded recovery diagnostics',{method:'tick',records:scanRecords,limit:34,repeats:2});
await add('scan continues past poison',{method:'tick',records:scanRecords.slice(31),limit:2,repeats:3});
await add('scan clears repaired issue',{method:'tick',operation:validIgnored,issues:[[operationId,'old_error']],limit:1,repeats:2});
await add('scan starts cancelled',{method:'tick',cancelled:true});
await add('scan permits independent issue during apply owner wait',{method:'tick',operation:validIgnored,issues:[[operationId,'old_error']],limit:1,independentIssue:true});
assert.ok(rows.length>=60);
process.stdout.write(JSON.stringify({root:rootId,rows}));
