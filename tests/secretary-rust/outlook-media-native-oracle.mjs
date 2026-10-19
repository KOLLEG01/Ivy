import assert from 'node:assert/strict';
import {mkdtemp,realpath,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,dirname,basename} from 'node:path';
import {load,owner as secretaryOwner,rootId,itemId,accepted as baseAccepted} from './inbox-fixtures.mjs';
import {outlookNativeCorpus} from './outlook-native-oracle.mjs';
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {SecretaryEngine,operationName,sourceName,identityHash,messageHash}=await load('services/secretary/src/engine.js');
const {OutlookSource}=await load('services/secretary/src/outlook-source.js');
const {OutlookMediaNative}=await load('services/secretary/src/outlook-media-native.js');
const {MediaSpool}=await load('services/secretary/src/media-spool.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {canonical,digest,hashJson,deriveOperationId,IvyError,parseOperationId}=await load('packages/sdk/src/node.js');
const template=outlookNativeCorpus.rows.find(row=>row.input.label==='original evidence reconstructs all calls').input;
const clone=structuredClone,at=new Date(outlookNativeCorpus.time).toISOString(),hash=value=>'sha256:'+value.repeat(64),rows=[];
const wall=outlookNativeCorpus.wall,previousNow=Date.now;Date.now=()=>wall;

async function add(label,c={}){
 const settings=clone(template.settings),configuration=clone(template.configuration),definition=settings.sources[0],principal=settings.identity.principalId;
 const source={schemaVersion:1,definition:clone(definition),definitionHash:hashJson(definition),cursor:null,checkpointOperation:null,updatedAt:null};
 const message={...clone(template.documents.find(row=>row.key==='secretary/outlook-evidence').value.page.messages[0]),attachments:'expected',...(c.inline?{nativeRevision:'outlook-inline-content:original'}:{})};
 const item={schemaVersion:1,sourceId:'mail',message,identityHash:identityHash('mail',message),messageHash:messageHash(message),media:[],decision:null,notice:null};
 const captureId='fixture-epoch:'+wall+':media-original-capture',itemPin={objectId:itemId,revision:1};
 const request={action:'capture',operationId:captureId,expectedScope:settings.identity.scope,sourceId:'mail',message};
 const first={...clone(baseAccepted),operationId:captureId,callerPrincipalId:principal,requestHash:hashJson(request),request,sourceId:'mail',source:clone(source),phase:'accepted',effect:null,existingEffect:itemPin,captureIdentity:item.identityHash};
 const capture={...clone(first),phase:'succeeded',effect:itemPin};
 const admission={sourceId:'mail',producerPrincipalId:principal,accountId:message.accountId,providerMessageId:'provider-original',itemObjectId:itemId,identityHash:item.identityHash,messageHash:item.messageHash};
 const methods=['account/read','thread/start','mcpServer/tool/call','thread/unsubscribe'];
 const plan={schemaVersion:1,admission,item:itemPin,captureOperationId:captureId,configuration,epoch:'old-epoch',createdAt:at,bindings:methods.map(method=>({method,definitionHash:hash('d'),outputSchema:{}}))};
 const input={label,settings,configuration,plan,spoolOwner:{scope:settings.identity.scope,producerPrincipalId:principal},locals:[{id:'source',key:'secretary/source',name:sourceName('mail'),revision:1,value:source,history:[{revision:1,value:source}]},{id:'capture',key:'secretary/operation',name:operationName(principal,captureId),revision:2,value:capture,history:[{revision:1,value:first},{revision:2,value:capture}]}],actions:[],documents:[],blobs:[],attempts:c.attempts??3,noCalls:c.noCalls??false};
 const file={id:'file',attachment_type:'fileAttachment',odata_type:'#microsoft.graph.fileAttachment',name:'inert.txt',content_type:'text/plain',size_bytes:4,is_inline:false,content_id:null,last_modified_date_time:null,payload_fetch_supported:true,payload_status:'not_requested',payload_error:null,file_uri:null};
 const attachments=c.empty?[]:[...Array.from({length:c.files??1},(_,index)=>({...clone(file),id:(c.files??1)>1?'file-'+index:file.id})),...(c.unsupported?[{...file,id:'reference',attachment_type:'referenceAttachment',odata_type:'#microsoft.graph.referenceAttachment',payload_fetch_supported:false}]:[])];
 const payload={message_id:admission.providerMessageId,attachment_id:'file',attachment_type:'fileAttachment',filename:file.name,size_bytes:file.size_bytes,mime_type:file.content_type,is_inline:false,content_id:null,payload_status:'materialized',file_uri:{file_name:file.name,mime_type:file.content_type,file_id:'original-file',download_url:'https://fixture.oaiusercontent.com/protected-original?sig=synthetic'}};
 if(c.badMaterialization)payload.attachment_id='other';
 const calls=[],operations=new Map(),trace=[],steps=[],downloads=[];let store,failedWrite=false,invoked=false;
 const target=configuration.target,status={...target,state:'ready',epoch:c.epoch??'old-epoch',pid:42,observedAt:at,code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:1000,bytes:1000,maximumBytes:1048576},observedMethods:[]};
 function invocation(slot){
  if(slot===0||slot===37)return{method:'account/read',params:{refreshToken:false}};
  if(slot===1)return{method:'thread/start',params:{cwd:configuration.threadCwd,ephemeral:true,permissions:':danger-full-access',approvalPolicy:'never'}};
  if(slot===38)return{method:'thread/unsubscribe',params:{threadId:'original-media-thread'}};
  const tool=slot===3?'list_attachments':slot>=4&&slot<=35?'fetch_attachment':'get_profile';
  return{method:'mcpServer/tool/call',params:{threadId:'original-media-thread',server:'codex_apps',tool:'microsoft_outlook_email.'+tool,arguments:slot===3?{message_id:admission.providerMessageId,content_mode:'metadata_only'}:tool==='fetch_attachment'?{message_id:admission.providerMessageId,attachment_id:attachments[slot-4]?.id??'unused'}:{}}};
 }
 const selected=[0,1,2,3,...attachments.flatMap((attachment,index)=>attachment.payload_fetch_supported?[4+index]:[]),36,37,38];
 for(const slot of selected){
  const original=invocation(slot),id='fixture-epoch:'+wall+':media-slot-'+slot;
  const selectedPayload=(c.files??1)>1&&slot>=4&&slot<=35?{...clone(payload),attachment_id:attachments[slot-4].id,file_uri:{...clone(payload.file_uri),file_id:'original-file-'+(slot-4),download_url:payload.file_uri.download_url+'&attachment='+String(slot-4)}}:payload;
  let result=slot===0||slot===37?{account:{type:'chatgpt',email:c.accountMismatch===slot?'foreign@example.test':'native@example.test'},requiresOpenaiAuth:true}:slot===1?{thread:{id:'original-media-thread'}}:slot===38?{status:c.cleanupStatus??'unsubscribed'}:{content:[],isError:false,structuredContent:slot===3?{message_id:admission.providerMessageId,attachments,errors:[],next_link:null}:slot>=4&&slot<=35?selectedPayload:c.profileMismatch===slot?{...configuration.profile,email:'foreign@example.test'}:configuration.profile};
  const operation={schemaVersion:1,operationId:id,callerPrincipalId:principal,serviceNodeId:target.serviceNodeId,nativeVersion:target.nativeVersion,nativeExecutableHash:target.nativeExecutableHash,...original,requestHash:hashJson(original),phase:c.pending===slot?'accepted':c.failed===slot?'failed':'succeeded',createdAt:at,updatedAt:at,epoch:c.pending===slot?null:'old-epoch',requestId:c.pending===slot?null:slot+42,reply:c.pending===slot?null:c.failed===slot?{error:{code:-1,message:'Native failed'}}:{result:clone(result)},code:c.pending===slot?null:c.failed===slot?'native_failed':null};
  const call={planHash:hashJson(plan),slot,...original,operationId:id,definitionHash:hash('d'),preparedAt:at};
  operations.set(id,operation);
  if(!c.noCalls)calls.push({call,seen:c.seen===slot?{...clone(operation),phase:'dispatched',reply:null,code:null}:null,observation:c.poll===slot||c.pending===slot||c.absent===slot||c.seen===slot?null:clone(operation)});
 }
 if(c.callMutation)c.callMutation(calls);
 const records=new Map(),pin=n=>({objectId:'00000000-0000-4000-8000-'+String(n).padStart(12,'0'),revision:1});let counter=200;
 const record=(p,key,name,content,references={},ownerObjectId=itemId)=>({id:p.objectId,key,name,content:clone(content),references:clone(references),ownerObjectId,parentId:rootId,currentRevision:p.revision});
 records.set(itemId,record(itemPin,'secretary/item','item-'+item.identityHash.slice(7),{encoding:'json',value:item},{},null));
 function raw(row){const json=row.content.encoding==='json',bytes=json?Buffer.from(canonical(row.content.value)):Buffer.from(row.content.value,'base64');return{object:{id:row.id,parentId:rootId,ownerObjectId:row.ownerObjectId,name:row.name,currentRevision:row.currentRevision,effectivelyArchived:false,contractKey:row.key},revision:{revision:row.currentRevision,contractVersion:row.key.startsWith('secretary/media-')?'1.0.0':contractVersion(row.key),mediaType:json?'application/json':'application/octet-stream',contentHash:json?hashJson(row.content.value):digest(bytes),byteLength:bytes.length,references:row.references,createdAt:at},content:clone(row.content)};}
 const binding=(namespace,name)=>({qualifiedName:namespace+'.'+name,definition:{namespace,name,interfaceVersion:namespace==='agent'?'1.0.0':target.nativeVersion,inputSchema:{},outputSchema:{}},definitionHash:hash(namespace==='agent'?'e':'d'),catalogHash:target.catalogHash});
 const client={async request(method,p,options){trace.push({method,params:clone(p),timeout:options?.timeoutMs??35000});try{let value;
  if(method==='serviceNodes.get')value={...secretaryOwner,...c.owner};
  else if(method==='system.status')value={runtimeEpoch:'fixture-epoch'};
  else if(method==='objects.query'){let values=[...records.values()].filter(row=>row.key===p.contractKey);for(const w of p.where.args)if(w.field==='object.name')values=values.filter(row=>row.name===w.value);value={items:values.map(row=>({objectId:row.id,revision:row.currentRevision})),nextCursor:null};}
  else if(method==='objects.read'){const row=records.get(p.objectId);if(!row)throw new IvyError('not_found','Fixture object missing.');value=raw(row);}
  else if(method==='objects.write'){const inject=!failedWrite&&c.writeFailure;if(inject){failedWrite=true;if(!c.afterWrite)throw new IvyError('fixture_write_failure','One original publication reply was lost.','unknown');}const row=record(pin(counter++),p.create.contractKey,p.create.name,p.content,p.references,p.create.ownerObjectId);records.set(row.id,row);if(inject)throw new IvyError('fixture_write_failure','One original publication reply was lost.','unknown');value={object:{id:row.id},revision:{revision:1}};}
  else if(method==='tools.list')value={provider:{node:{serviceNodeId:target.serviceNodeId}},items:[binding(p.namespace,p.namePrefix)],nextCursor:null};
  else if(method==='tools.call'){
   if(p.qualifiedName==='agent.status')value=status;
   else if(p.qualifiedName==='agent.operation'){
    if(c.noCalls)throw new IvyError('fixture_cut','Stopped after original call preparation.','unknown');
    const id=p.arguments.operationId,slot=Number(id.slice(id.lastIndexOf('-')+1));if(c.absent===slot&&!invoked)throw new IvyError('not_found','Original operation absent.','not_executed',{kind:'agent_operation_absent',serviceNodeId:target.serviceNodeId,operationId:id,epoch:status.epoch});value=operations.get(id);assert(value);
   }else if(p.qualifiedName==='agent.invoke'){invoked=true;value=operations.get(p.arguments.operationId);assert(value);if(c.uncertainSend)throw new IvyError('fixture_send_failure','Original native send is uncertain.','unknown');}
   else assert.fail(p.qualifiedName);
  }else assert.fail('Unexpected media RPC '+method);
  steps.push({method,value:clone(value)});return clone(value);
 }catch(cause){const error=IvyError.from(cause);steps.push({method,error:{code:error.code,message:error.message,outcome:error.outcome,...(error.details===undefined?{}:{details:error.details})}});throw cause;}}};
 store=new SecretaryStore(client,rootId);for(const row of input.locals){store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(row.id,row.key,row.name,row.revision,0,row.key==='secretary/operation'?1:null,canonical(row.value));for(const revision of row.history)store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(row.id,revision.revision,canonical(revision.value));}
 const abort=new AbortController(),engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings,epoch:Promise.resolve('fixture-epoch'),owner:{serviceNodeId:outlookNativeCorpus.nodeId},signal:abort.signal,now:()=>new Date(at)});
 const directory=await mkdtemp(join(tmpdir(),'secretary-media-native-reference-')),parent=await realpath(tmpdir());let spool;
 try{
  spool=await MediaSpool.open(directory,input.spoolOwner);if(!c.omitPlan)spool.prepareRead(plan);
  for(const row of calls)spool.db.prepare('INSERT INTO read_calls VALUES(?,?,?,?,?)').run(hashJson(plan),row.call.slot,canonical(row.call),row.seen===null?null:canonical(row.seen),row.observation===null?null:canonical(row.observation));
  input.spool={calls:clone(calls),plan:c.omitPlan?null:clone(plan)};
  const worker=new OutlookMediaNative(new OutlookSource(engine,configuration),spool,async(url,options)=>{downloads.push({url:String(url),method:options.method,redirect:options.redirect,credentials:options.credentials,headers:options.headers});if(c.downloadError)throw Error('One original binary response was interrupted.');return new Response(Uint8Array.from([0,1,254,255]),{status:c.httpStatus??200,headers:{'content-type':'application/octet-stream','content-length':'4'}});});
  const results=[];
  if(c.prepare){input.actions.push({method:'prepare'});try{results.push({value:await worker.prepare(admission,itemPin,captureId)});}catch(cause){const error=IvyError.from(cause);results.push({code:error.code,message:error.message,outcome:error.outcome});}}
  if(c.cancelled)abort.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
  input.cancelled=c.cancelled??false;
  for(let n=0;n<input.attempts;n++){input.actions.push({method:'step'});try{results.push({value:await worker.step(plan)});}catch(cause){const error=IvyError.from(cause);results.push({code:error.code,message:error.message,outcome:error.outcome,...(error.details===undefined?{}:{details:error.details})});}}
  const complete=results.findLast(result=>result.value?.phase==='complete');
  if(c.verify&&complete){const coverage=clone(complete.value.coverage);if(c.coverage)c.coverage(coverage);input.actions.push({method:'verify',coverage});try{await worker.verifyCoverage(plan,coverage);results.push({value:null});}catch(cause){const error=IvyError.from(cause);results.push({code:error.code,message:error.message,outcome:error.outcome});}}
  const attempts=spool.db.prepare('SELECT * FROM attempts ORDER BY operation_id').all().map(row=>({operationId:row.operation_id,intent:JSON.parse(row.intent),materialization:JSON.parse(row.materialization),phase:row.phase,evidence:row.evidence===null?null:JSON.parse(row.evidence),bytes:row.payload===null?null:Buffer.from(row.payload).toString('base64'),errorCode:row.error_code}));
  input.downloads=downloads.map(entry=>{const attempt=attempts.find(attempt=>attempt.materialization.downloadUrl===entry.url);assert(attempt);return{materialization:attempt.materialization,...(attempt.evidence?{bytes:attempt.bytes,evidence:attempt.evidence}:{errorCode:attempt.errorCode})};});
  const savedCalls=spool.db.prepare('SELECT * FROM read_calls ORDER BY plan_hash,slot').all().map(row=>({call:JSON.parse(row.value),seen:row.seen===null?null:JSON.parse(row.seen),observation:row.observation===null?null:JSON.parse(row.observation)}));
  const docs=store.db.prepare('SELECT id,key,name,current_revision,value_json FROM technical_documents ORDER BY key,name').all();
  const retained={calls:savedCalls,attempts,plan:spool.readPlan(itemPin),usedBytes:spool.usedBytes(),technical:docs.map(row=>({id:row.id,key:row.key,name:row.name,revision:row.current_revision,value:JSON.parse(row.value_json)}))};
  const ids=new Map(docs.map(row=>[row.id,'document:'+row.key+':'+row.name]));
  if(c.noCalls)for(const row of savedCalls){assert.equal(parseOperationId(row.call.operationId).runtimeEpoch,'fixture-epoch');ids.set(row.call.operationId,'fresh-media-slot-'+row.call.slot);}
  const normalize=value=>typeof value==='string'?(ids.get(value)??value):Array.isArray(value)?value.map(normalize):value&&typeof value==='object'?Object.fromEntries(Object.entries(value).map(([key,value])=>[key,normalize(value)])):value;
  const expected=normalize({results,trace,retained});for(const row of expected.trace)if(row.method==='objects.write'){assert.equal(parseOperationId(row.params.mutationId).runtimeEpoch,'fixture-epoch');row.params.mutationId='<new-operation>';}
  input.steps=steps;rows.push({input,expected});
 }finally{
  spool?.close();store.close();const actual=await realpath(directory);assert.equal(dirname(actual),parent);assert(basename(actual).startsWith('secretary-media-native-reference-'));await rm(actual,{recursive:true});
 }
}
try{
 await add('acquire once then publish after both account gates',{verify:true});
 await add('two complete original acquisitions publish distinct manifests',{files:2,attempts:4,verify:true});
 await add('all thirty-two attachment slots finish before profile and cleanup',{files:32,attempts:34,verify:true});
 await add('unsupported attachment retains explicit coverage gap',{unsupported:true,verify:true});
 await add('empty attachment list still requires cleanup',{empty:true,verify:true});
 await add('inline original content remains an explicit whole gap',{inline:true,verify:true});
 for(const slot of [0,37])await add('account mismatch '+slot,{accountMismatch:slot,verify:true});
 for(const slot of [2,36])await add('profile mismatch '+slot,{profileMismatch:slot,verify:true});
 for(const slot of [0,1,3,4,38])await add('native failure '+slot,{failed:slot,verify:true});
 await add('unsuccessful cleanup never certifies coverage',{cleanupStatus:'stillLoaded'});
 await add('notLoaded cleanup is complete',{cleanupStatus:'notLoaded',verify:true});
 await add('materialization mismatch stays attachment specific',{badMaterialization:true,verify:true});
 await add('interrupted bytes remain an explicit acquisition failure',{downloadError:true,verify:true});
 await add('expired materialization remains an explicit acquisition failure',{httpStatus:403,verify:true});
 await add('pending materialization performs no GET',{pending:4});
 await add('one original missing observation is reconciled',{poll:4,verify:true});
 await add('confirmed absence permits original guarded dispatch',{absent:4,verify:true});
 await add('uncertain send recovers the original materialization',{absent:4,uncertainSend:true,verify:true});
 await add('new epoch closes original read without replacement bytes',{absent:4,epoch:'new-epoch',verify:true});
 await add('seen work cannot become confirmed absent',{absent:4,seen:4});
 await add('fresh native call is durable before polling',{noCalls:true,attempts:1});
 await add('prepare verifies existing original plan',{prepare:true,attempts:0});
 await add('prepare captures new exact bindings',{prepare:true,omitPlan:true,noCalls:true,attempts:0});
 await add('owner refusal prevents acquisition',{owner:{principalId:'other'}});
 await add('cancellation prevents acquisition',{cancelled:true});
 for(const afterWrite of [false,true])await add('publication reply loss '+afterWrite,{writeFailure:true,afterWrite,verify:true,attempts:4});
 await add('coverage admission cannot change',{verify:true,coverage:value=>{value.admission.accountId='other';}});
 await add('coverage final observation cannot change',{verify:true,coverage:value=>{value.observedAt='2026-09-08T09:00:00.001Z';}});
 await add('coverage may not invent attachment gaps',{verify:true,coverage:value=>{value.attachments[0].manifest=null;value.attachments[0].errorCode='invented';}});
 await add('retained native invocation cannot change',{callMutation:rows=>{rows.find(row=>row.call.slot===0).call.params.refreshToken=true;}});
 for(const name of ['acquire once then publish after both account gates','unsupported attachment retains explicit coverage gap','empty attachment list still requires cleanup','inline original content remains an explicit whole gap','new epoch closes original read without replacement bytes'])assert(rows.find(row=>row.input.label===name).expected.results.some(result=>result.value?.phase==='complete'),name);
 assert.equal(rows.find(row=>row.input.label==='acquire once then publish after both account gates').input.downloads.length,1);
 for(const [label,count]of [['two complete original acquisitions publish distinct manifests',2],['all thirty-two attachment slots finish before profile and cleanup',32]]){
  const row=rows.find(row=>row.input.label===label);assert.equal(row.input.downloads.length,count);const complete=row.expected.results.find(result=>result.value?.phase==='complete');assert(complete,label);assert.equal(complete.value.coverage.attachments.length,count);assert(complete.value.coverage.attachments.every(entry=>entry.manifest&&entry.errorCode===null),label);
 }
 console.log(JSON.stringify({rootId,nodeId:outlookNativeCorpus.nodeId,time:outlookNativeCorpus.time,wall,rows}));
}finally{Date.now=previousNow;}
