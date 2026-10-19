import assert from 'node:assert/strict';
import {mkdtemp,realpath,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,dirname,basename} from 'node:path';
import {load,owner,rootId,itemId} from './inbox-fixtures.mjs';
import {outlookNativeCorpus as native} from './outlook-native-oracle.mjs';
import {outlookMediaNativeCase} from './outlook-media-native-oracle.mjs';
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {OutlookSource}=await load('services/secretary/src/outlook-source.js');
const {OutlookMediaSource}=await load('services/secretary/src/outlook-media-source.js');
const {MediaSpool}=await load('services/secretary/src/media-spool.js');
const {outlookPage}=await load('services/secretary/src/outlook-projection.js');
const {prepareMediaInput}=await load('services/secretary/src/media-input.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {canonical,digest,hashJson,IvyError}=await load('packages/sdk/src/node.js');
const clone=structuredClone,rows=[],at=new Date(native.time).toISOString(),beforeNow=Date.now;
Date.now=()=>native.wall;
const template=native.rows.find(row=>row.input.label==='original evidence reconstructs all calls').input;
const outcome=cause=>{const error=IvyError.from(cause);return{code:error.code,outcome:error.outcome};};
async function add(label,c={}){
 const original=clone(template),list=original.documents.find(row=>row.key==='secretary/outlook-call'&&row.value.slot===3);
 const blob=original.blobs.find(row=>row.id===list.value.observation.chunks[0].objectId),operation=JSON.parse(Buffer.from(blob.bytes,'base64'));
 operation.reply.result.structuredContent.value[0].has_attachments=true;
 const bytes=Buffer.from(canonical(operation));blob.bytes=bytes.toString('base64');list.value.observation={byteLength:bytes.length,contentHash:digest(bytes),chunks:[{objectId:blob.id,revision:1}]};
 const proof=original.documents.find(row=>row.key==='secretary/outlook-evidence'),originalPlan=original.documents.find(row=>row.key==='secretary/outlook-plan');
 proof.value.page=outlookPage(operation.reply.result.structuredContent,original.configuration.profile,original.settings.sources[0],originalPlan.value.selection,at);
 const fixture=await outlookMediaNativeCase(label,{message:proof.value.page.messages[0],attempts:0,files:c.files??1,unsupported:c.unsupported,inline:c.inline,pending:c.pending,cleanupStatus:c.cleanupStatus});
 const input=fixture.input;
 input.documents=original.documents;input.blobs=original.blobs;input.actions=[];input.options=c;
 input.documents.push({id:'publication-0008',key:'secretary/outlook-publication',name:'media-source-publication',revision:2,value:{schemaVersion:1,evidence:{objectId:proof.id,revision:proof.revision},captured:[{objectId:'capture',revision:2}],checkpoint:null}});
 for(let index=0;index<(c.leadingEmpty??0);index++){const publication=clone(input.documents.at(-1));publication.id='publication-'+String(index).padStart(4,'0');publication.name='older-'+index;publication.revision=1;publication.value.captured=[];input.documents.push(publication);}
 const records=new Map(),history=new Map(),pin=n=>({objectId:'00000000-0000-4000-8000-'+String(n).padStart(12,'0'),revision:1});let counter=200,store,failedWrite=false,trace=[],steps=[],downloads=[];
 const item=clone(input.locals.find(row=>row.id==='capture').value.request.message);
 const initialItem={schemaVersion:1,sourceId:'mail',message:item,identityHash:input.plan.admission.identityHash,messageHash:input.plan.admission.messageHash,media:[],decision:null,notice:null};
 const record=(p,key,name,content,references={},ownerObjectId=itemId)=>({id:p.objectId,key,name,content:clone(content),references:clone(references),ownerObjectId,currentRevision:p.revision});
 records.set(itemId,record(input.plan.item,'secretary/item','item-'+initialItem.identityHash.slice(7),{encoding:'json',value:initialItem},{},null));
 function raw(row){const json=row.content.encoding==='json',bytes=json?Buffer.from(canonical(row.content.value)):Buffer.from(row.content.value,'base64');return{object:{id:row.id,parentId:rootId,ownerObjectId:row.ownerObjectId,name:row.name,currentRevision:records.get(row.id).currentRevision,effectivelyArchived:false,contractKey:row.key},revision:{revision:row.currentRevision,contractVersion:row.key.startsWith('secretary/media-')?'1.0.0':contractVersion(row.key),mediaType:json?'application/json':'application/octet-stream',contentHash:json?hashJson(row.content.value):digest(bytes),byteLength:bytes.length,references:row.key==='secretary/item'?store.objectWriteRequest(row.key,row.content.value,{object:{objectId:row.id,revision:row.currentRevision}},input.plan.captureOperationId).references:row.references,createdAt:at},content:clone(row.content)};}
 history.set(itemId+':1',clone(records.get(itemId)));
 let mutation=null,spool,afterAttachTampered=false;
 const target=input.configuration.target,hash=value=>'sha256:'+value.repeat(64);
 const binding=(namespace,name)=>({qualifiedName:namespace+'.'+name,definition:{namespace,name,interfaceVersion:namespace==='agent'?'1.0.0':target.nativeVersion,inputSchema:{},outputSchema:{}},definitionHash:hash(namespace==='agent'?'e':'d'),catalogHash:target.catalogHash});
 const client={async request(method,p,options){trace.push({method,params:clone(p),timeout:options?.timeoutMs??35000});try{let value;
  if(method==='serviceNodes.get')value={...owner,...c.owner};
  else if(method==='system.status')value={runtimeEpoch:'fixture-epoch'};
  else if(method==='objects.query'){let selected=[...records.values()].filter(row=>row.key===p.contractKey);for(const where of p.where.args)if(where.field==='object.name')selected=selected.filter(row=>row.name===where.value);value={items:selected.map(row=>({objectId:row.id,revision:row.currentRevision})),nextCursor:null};}
  else if(method==='objects.read'){let row=p.revision?history.get(p.objectId+':'+p.revision):records.get(p.objectId);if(!row)throw new IvyError('not_found','Fixture object missing.');row=clone(row);if(mutation?.path&&mutation.key===row.key){const keys=mutation.path.split('.'),last=keys.pop();let current=row;for(const key of keys)current=current[key];current[last]=clone(mutation.value);}value=raw(row);if(mutation?.raw&&mutation.key===row.key){const keys=mutation.raw.split('.'),last=keys.pop();let current=value;for(const key of keys)current=current[key];current[last]=clone(mutation.value);}}
  else if(method==='objects.write'){
   const key=p.create?.contractKey??'secretary/item',inject=!failedWrite&&c.writeKey===key;
   if(inject){failedWrite=true;if(!c.applied)throw new IvyError('fixture_write_failure','Original publication reply lost.','unknown');}
   let row;if(p.create){row=record(pin(counter++),key,p.create.name,p.content,p.references,p.create.ownerObjectId);records.set(row.id,row);}else{row=records.get(p.objectId);assert(row);assert.equal(p.expectedRevision,row.currentRevision);row.currentRevision++;row.content=clone(p.content);row.references=clone(p.references);}
   history.set(row.id+':'+row.currentRevision,clone(row));
   if(key==='secretary/item'&&c.afterAttachTamper&&!afterAttachTampered){afterAttachTampered=true;const saved=spool.db.prepare('SELECT observation FROM read_calls WHERE slot=38').get(),observation=JSON.parse(saved.observation);observation.reply.result.status='stillLoaded';spool.db.prepare('UPDATE read_calls SET observation=? WHERE slot=38').run(canonical(observation));}
   if(inject)throw new IvyError('fixture_write_failure','Original publication reply lost.','unknown');value={object:{id:row.id},revision:{revision:row.currentRevision}};
  }else if(method==='tools.list')value={provider:{node:{serviceNodeId:target.serviceNodeId}},items:[binding(p.namespace,p.namePrefix)],nextCursor:null};
  else if(method==='tools.call'){
   if(p.qualifiedName==='agent.status')value={...target,state:'ready',epoch:'old-epoch',pid:42,observedAt:at,code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:1000,bytes:1000,maximumBytes:1048576},observedMethods:[]};
   else if(p.qualifiedName==='agent.operation'){
    const saved=input.spool.calls.find(row=>row.call.operationId===p.arguments.operationId);assert(saved);const call=saved.call;
    value={schemaVersion:1,operationId:call.operationId,callerPrincipalId:input.settings.identity.principalId,serviceNodeId:target.serviceNodeId,nativeVersion:target.nativeVersion,nativeExecutableHash:target.nativeExecutableHash,method:call.method,params:call.params,requestHash:hashJson({method:call.method,params:call.params}),phase:'accepted',createdAt:at,updatedAt:at,epoch:null,requestId:null,reply:null,code:null};
   }else assert.fail('Unexpected tool '+p.qualifiedName);
  }else assert.fail('Unexpected media source RPC '+method);
  steps.push({method,value:clone(value)});return clone(value);
 }catch(cause){const error=IvyError.from(cause);steps.push({method,error:{code:error.code,message:error.message,outcome:error.outcome}});throw cause;}}};
 store=new SecretaryStore(client,rootId);
 for(const row of input.locals){store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(row.id,row.key,row.name,row.revision,0,row.key==='secretary/operation'?1:null,canonical(row.value));for(const revision of row.history)store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(row.id,revision.revision,canonical(revision.value));}
 for(const row of input.documents){store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(row.id,row.key,row.name,row.revision,canonical(row.value));store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(row.id,row.revision,canonical(row.value));}
 for(const row of input.blobs)store.db.prepare('INSERT INTO technical_blobs VALUES(?,?,?,?)').run(row.id,row.key,row.name,Buffer.from(row.bytes,'base64'));
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings:input.settings,epoch:Promise.resolve('fixture-epoch'),owner:{serviceNodeId:native.nodeId},signal:new AbortController().signal,now:()=>new Date(at)});
 const directory=await mkdtemp(join(tmpdir(),'secretary-media-source-reference-')),parent=await realpath(tmpdir());
 try{
  spool=await MediaSpool.open(directory,input.spoolOwner);spool.prepareRead(input.plan);
  for(const row of input.spool.calls)spool.db.prepare('INSERT INTO read_calls VALUES(?,?,?,?,?)').run(hashJson(input.plan),row.call.slot,canonical(row.call),row.seen===null?null:canonical(row.seen),row.observation===null?null:canonical(row.observation));
  let rawDownloads=[];const request=async url=>{rawDownloads.push(String(url));return new Response(Buffer.from('safe'),{headers:{'content-length':'4','content-type':'text/plain'}});};
  const prepared=[];const prepare=async(source,bytes,budget)=>{prepared.push({source:clone(source),bytes:bytes.toString('base64'),budget});return prepareMediaInput(source,bytes,budget);};
  let worker=new OutlookMediaSource(new OutlookSource(engine,input.configuration),spool,request,prepare),context=null;const results=[];
  const snapshot=()=>({plans:spool.db.prepare('SELECT COUNT(*) AS n FROM read_plans').get().n,calls:spool.db.prepare('SELECT COUNT(*) AS n FROM read_calls').get().n,attempts:spool.db.prepare('SELECT COUNT(*) AS n FROM attempts').get().n,usedBytes:spool.usedBytes()});
  const attemptSnapshot=()=>spool.db.prepare('SELECT * FROM attempts').all().map(row=>({materialization:JSON.parse(row.materialization),evidence:row.evidence===null?null:JSON.parse(row.evidence),bytes:row.payload===null?null:Buffer.from(row.payload).toString('base64'),errorCode:row.error_code}));
  for(let n=0;n<(c.attempts??3);n++){
   input.actions.push({method:'step'});try{results.push({value:await worker.step()});}catch(cause){results.push(outcome(cause));}
   for(const url of rawDownloads.splice(0)){const attempt=attemptSnapshot().find(row=>row.materialization.downloadUrl===url);assert(attempt);downloads.push(clone(attempt));}
   results.push({spool:snapshot(),issue:worker.issue});
   if(c.restartEach)worker=new OutlookMediaSource(new OutlookSource(engine,input.configuration),spool,request,prepare);
   if(c.restart&&n===(c.restartAt??0)){input.actions.push({method:'restart'});spool.close();spool=await MediaSpool.open(directory,input.spoolOwner);worker=new OutlookMediaSource(new OutlookSource(engine,input.configuration),spool,request,prepare);}
  }
  if(c.context!==false){
   if(c.mutation==='owner')mutation={key:'secretary/media-coverage',raw:'object.ownerObjectId',value:'foreign'};
   if(c.mutation==='source')mutation={key:'secretary/media-coverage',path:'content.value.admission.sourceId',value:'foreign'};
   if(c.mutation==='linkHash')mutation={key:'secretary/item',path:'content.value.media.0.contentHash',value:'sha256:'+'f'.repeat(64)};
   if(c.mutation==='encoding')mutation={key:'secretary/media-coverage',raw:'content.encoding',value:'base64'};
   input.actions.push({method:'context'});try{context=await worker.context(input.plan.item);results.push({value:context?{item:context.item,coverage:context.coverage}:null});}catch(cause){results.push(outcome(cause));}
   if(context){
    if(c.mutation==='changedCoverage')mutation={key:'secretary/media-coverage',path:'content.value.observedAt',value:'2026-09-08T09:00:00.001Z'};
    if(c.mutation==='manifestIdentity')mutation={key:'secretary/media-manifest',path:'content.value.evidence.attachment.name',value:'foreign'};
    if(c.mutation==='bytes')mutation={key:'secretary/media-chunk',path:'content.value',value:'AQIDBA=='};
    const index=c.index??0;input.actions.push({method:'attachment',index});try{const payload=await context.readAttachment(index);results.push({value:payload?{pin:payload.pin,value:payload.value,bytes:payload.bytes.toString('base64')}:null});}catch(cause){results.push(outcome(cause));}
    input.actions.push({method:'input',index,budget:c.budget??1024});try{results.push({value:await context.input(index,c.budget??1024)});}catch(cause){results.push(outcome(cause));}
   }
  }
  const retained={spool:snapshot(),core:store.db.prepare('SELECT key,name,current_revision,value_json FROM documents ORDER BY key,name').all().map(row=>({key:row.key,name:row.name,revision:row.current_revision,value:JSON.parse(row.value_json)}))};
  input.downloads=downloads;input.steps=steps;
  for(const row of trace)if(row.method==='objects.write')row.params.mutationId='<new-operation>';
  const expected={results,trace,prepared,retained};rows.push({input,expected});
 }finally{spool?.close();store.close();const actual=await realpath(directory);assert.equal(dirname(actual),parent);assert(basename(actual).startsWith('secretary-media-source-reference-'));await rm(actual,{recursive:true});}
}
try{
 await add('save attach verify retire and read complete original input',{attempts:2});
 await add('retained pending acquisition reopens without replacement GET',{restart:true});
 await add('retired spool reopens and original context remains readable',{restart:true,restartAt:1});
 await add('two original files retain distinct context',{files:2,attempts:4,index:1});
 await add('unsupported attachment remains explicit and retires',{unsupported:true,index:1});
 await add('inline gap is complete and retires without bytes',{inline:true,index:0});
 await add('unsuccessful native cleanup keeps original spool',{cleanupStatus:'stillLoaded',context:false});
 await add('verification after attachment must succeed before retirement',{afterAttachTamper:true,context:false});
 await add('pending materialization never publishes or retires',{pending:4,context:false});
 await add('owner mismatch prevents source selection',{owner:{principalId:'other'},context:false});
 for(const writeKey of ['secretary/media-coverage','secretary/item'])for(const applied of [false,true])await add('uncertain '+writeKey+' '+applied,{writeKey,applied,attempts:4});
 await add('original restart loses unattached saved coverage',{writeKey:'secretary/item',applied:false,restart:true,restartAt:1,attempts:4});
 for(const mutation of ['owner','source','linkHash','encoding','changedCoverage','manifestIdentity','bytes'])await add('context refuses '+mutation,{mutation});
 for(const index of [-1,0.5,99])await add('original attachment index '+index,{index});
 await add('input enforces original request budget',{budget:1});
 await add('input rejects invalid budget',{budget:0});
 for(const label of ['save attach verify retire and read complete original input','retained pending acquisition reopens without replacement GET','two original files retain distinct context','unsupported attachment remains explicit and retires']){
  const row=rows.find(row=>row.input.label===label);assert(row.expected.results.some(result=>result.value==='complete'),label);assert.equal(row.expected.retained.spool.plans,0,label);assert.equal(row.expected.retained.spool.calls,0,label);assert.equal(row.expected.retained.spool.attempts,0,label);
 }
 await add('original first eight publications starve ninth after restart',{leadingEmpty:8,restartEach:true,context:false});
 const fairScanOriginal=rows.pop();assert(fairScanOriginal.expected.results.filter(result=>'value'in result).every(result=>result.value==='idle'));assert.equal(fairScanOriginal.input.downloads.length,0);assert.equal(fairScanOriginal.expected.retained.spool.plans,1);
 const unattached=rows.find(row=>row.input.label==='original restart loses unattached saved coverage');assert.equal(unattached.expected.retained.spool.plans,0);assert.equal(unattached.expected.results.at(-1).value,null);assert.equal(unattached.expected.retained.core.filter(row=>row.value.action==='attach').length,1);assert(unattached.expected.retained.core.some(row=>row.value.action==='attach'&&row.value.phase==='accepted'));
 console.log(JSON.stringify({rootId,nodeId:native.nodeId,time:native.time,wall:native.wall,rows,fairScanOriginal}));
}finally{Date.now=beforeNow;}
