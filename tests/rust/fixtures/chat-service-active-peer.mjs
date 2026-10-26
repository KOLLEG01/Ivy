// Public Hive/Agent protocol fixture. The real native Chat service owns every
// local transition; this peer observes its journal through a read-only handle.
import './chat-store-loader.mjs';
import {readFileSync,writeFileSync,mkdtempSync,rmSync,existsSync} from 'node:fs';
import {join} from 'node:path';
import {tmpdir} from 'node:os';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {setTimeout as delay} from 'node:timers/promises';
import {WebSocketServer} from 'ws';
import {DatabaseSync} from 'node:sqlite';
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {canonical,digest,hashJson}=await import('../../../packages/contracts/src/canonical.ts');
const {checkedNativeContract}=await import('../../../packages/contracts/src/checked-native-contract.ts');
const {parseOperationId}=await import('../../../packages/contracts/src/operation-id.ts');
const {toolDefinitionHash}=await import('../../../packages/contracts/src/tool-definition.ts');
const clone=v=>structuredClone(v),at=new Date(1790000000123).toISOString();
const error=e=>{e=IvyError.from(e);return{code:e.code,message:e.message,outcome:e.outcome,...(e.details===undefined?{}:{details:e.details})};};

function environment(seed,kind){
 const root=mkdtempSync(join(tmpdir(),'ivy-main-original-')),file=join(root,'chat-bridge.sqlite');
 writeFileSync(file,Buffer.from(seed.database,'base64'));
 const observer=new DatabaseSync(file,{readOnly:true}),definition=clone(seed.definition);
 const source=checkedNativeContract(definition.nativePlan.nativeVersion,{sourceHash:definition.nativePlan.catalogSourceHash});
 const controls={epoch:'original-epoch',phase:'succeeded',...clone(seed.controls)},records=clone(seed.records),journal=new Map(clone(seed.journal)),events=[],receipts=new Map(),aliases=new Map(),mutations=new Map();
 let reads=0,turns=0,starts=seed.starts??0;
 const mainName='Chat Main '+hashJson({principalId:definition.principalId,workspaceId:definition.workspaceId,definitionHash:hashJson(definition)}).slice(7);
 function local(key,reference,parentId=null){
  const id=typeof reference==='string'?reference:reference.objectId,revision=typeof reference==='string'?null:reference.revision;
  const row=observer.prepare('SELECT d.id,d.parent_id,r.revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=COALESCE(?,d.current_revision) WHERE d.key=? AND d.id=?').get(revision,key,id);
  if(!row||row.parent_id!==parentId)throw Error('Missing durable claim '+key+' '+id);
  return{pin:{objectId:row.id,revision:row.revision},value:JSON.parse(row.value_json)};
 }
 function main(){
  const row=observer.prepare("SELECT id FROM documents WHERE key='chat-bridge/main' AND name=? AND parent_id IS NULL").get(mainName);
  return row?local('chat-bridge/main',row.id):null;
 }
 function matches(record,predicate){
  if(predicate.op==='and')return predicate.args.every(p=>matches(record,p));
  if(predicate.op==='or')return predicate.args.some(p=>matches(record,p));
  let value;if(predicate.field.startsWith('object.'))value=record.object[predicate.field.slice(7)];
  else if(predicate.field.startsWith('data:/')){value=record.content.value;for(const part of predicate.field.slice(6).split('/'))value=value?.[part.replaceAll('~1','/').replaceAll('~0','~')];}
  if(!['isNull','eq'].includes(predicate.op))throw Error('Unexpected public query '+predicate.op);
  return predicate.op==='isNull'?value==null:JSON.stringify(value)===JSON.stringify(predicate.value);
 }
 const thread=id=>({cliVersion:source.catalog.version,createdAt:1788690000,cwd:'/synthetic/project',ephemeral:false,id,modelProvider:'openai',preview:'Synthetic Main',projectId:null,sessionId:id,source:'appServer',status:{type:controls.threadStatus??'idle',...(controls.threadStatus==='active'?{activeFlags:[]}: {})},turns:[],updatedAt:1788690000,canAcceptDirectInput:controls.directInput??true,name:'Synthetic'});
 function operation(args,reply,phase='succeeded'){
  return{schemaVersion:1,operationId:args.operationId,callerPrincipalId:'chat-service',serviceNodeId:'owner',nativeVersion:source.catalog.version,nativeExecutableHash:source.catalog.nativeExecutableHash,method:args.method,params:clone(args.params),requestHash:hashJson({method:args.method,params:args.params}),phase,createdAt:at,updatedAt:at,epoch:controls.epoch,requestId:'delivery-native-'+(journal.size+1),code:phase==='failed'?'native_error':null,reply:phase==='dispatched'?null:reply};
 }
 function assertClaim(method,p,event){
  if(method!=='tools.call'||p.qualifiedName!=='agent.invoke')return;
  const args=p.arguments,selected=main();
  if(args.method==='thread/start'||args.method==='thread/resume'&&selected?.value.pendingAction){
   if(!selected?.value.pendingAction)throw Error('No retained Main claim before invoke');
   const op=local('chat-bridge/operation',selected.value.pendingAction),call=local('chat-bridge/native-call',op.value.nativeCall,op.pin.objectId);
   if(op.value.phase!=='applying'||call.value.operationId!==args.operationId||selected.value.queue.length)throw Error('Native invoke preceded its durable attachment');
   event.assertClaim=true;return;
  }
  const ticket=selected?.value.queue[0];if(!ticket||selected.value.pendingAction)throw Error('No current unclaimed queue head before invoke');
  const record=records.find(r=>r.object.id===ticket.inputId),input=record?.content.value,slot=args.method==='turn/start'?'turn':args.method==='turn/interrupt'?'interrupt':'resume';
  if(!input?.nativeCalls[slot])throw Error('Native delivery preceded its saved input attachment');
  const call=local('chat-bridge/native-call',input.nativeCalls[slot],record.object.id);
  if(call.value.operationId!==args.operationId||(slot!=='interrupt'&&input.cancellation)||(slot==='interrupt'&&!input.cancellation))throw Error('The saved original input does not authorize this native call');
  event.assertInputClaim={inputId:record.object.id,slot};
 }
 async function rpc(method,p){
  if(method==='system.status')return{runtimeEpoch:'chat'};
  if(method==='settings.get')return{settings:{quietHours:null}};
  if(method==='system.diagnostics')return{items:[],nextCursor:null};
  if(method==='serviceNodes.get')return{principalId:'chat-service',serviceName:'chat-bridge',connected:true,synced:true,...controls.node};
  if(method==='objects.query')return{items:records.filter(r=>r.object.contractKey===p.contractKey&&(p.includeArchived||!r.object.effectivelyArchived)&&matches(r,p.where)).map(r=>({objectId:r.object.id,revision:r.revision.revision})),nextCursor:null};
  if(method==='objects.read'||method==='objects.stat'){
   const record=records.find(r=>r.object.id===p.objectId);if(!record)throw new IvyError('object_missing','Original public record is unavailable.');return clone(method==='objects.stat'?record.object:record);
  }
  if(method==='objects.write'){
   if(parseOperationId(p.mutationId).runtimeEpoch!=='chat')throw Error('Wrong mutation epoch');
   if(receipts.has(p.mutationId))return clone(receipts.get(p.mutationId));
   let record;if(p.create){
    if(records.some(r=>r.object.contractKey===p.create.contractKey&&r.object.parentId===p.create.parentId&&r.object.name===p.create.name))throw new IvyError('revision_conflict','Original public name exists.');
    record={object:{id:'main-public-'+(records.length+1),parentId:p.create.parentId,ownerObjectId:p.create.ownerObjectId,name:p.create.name,contractKey:p.create.contractKey,effectivelyArchived:false},revision:{revision:1,contractVersion:p.contractVersion,mediaType:'application/json'},content:clone(p.content)};records.push(record);
   }else{
    record=records.find(r=>r.object.id===p.objectId);if(!record)throw new IvyError('object_missing','Original public record is unavailable.');
    if(record.revision.revision!==p.expectedRevision)throw new IvyError('revision_conflict','Original public revision changed.');record.revision.revision++;record.content=clone(p.content);
   }
   receipts.set(p.mutationId,clone(record));
   if(p.content.encoding!=='json'){
    const bytes=p.content.encoding==='base64'?Buffer.from(p.content.value,'base64'):Buffer.from(p.content.value);
    Object.assign(record.revision,{mediaType:p.create?.contractKey==='chat-bridge/image-png'?'image/png':'text/plain',byteLength:bytes.length,contentHash:digest(bytes)});
   }
   return clone(record);
  }
  if(method==='tools.list'){
   const namespace=p.namespace,name=p.namePrefix,tool=namespace==='codex'?source.definitions.get(name):{namespace:'agent',name,interfaceVersion:'1.0.0',inputSchema:{},outputSchema:{}};
   return{provider:{node:{serviceNodeId:'owner'}},items:[{qualifiedName:namespace+'.'+name,definition:tool,definitionHash:hashJson(tool),catalogHash:source.catalogHash}],nextCursor:null};
  }
  if(method==='tools.call'){
   const name=p.name??p.qualifiedName?.split('.').slice(1).join('.'),args=p.arguments;
   if(name==='status')return{serviceNodeId:'owner',hostId:'synthetic-native',nativeVersion:source.catalog.version,nativeExecutableHash:source.catalog.nativeExecutableHash,catalogHash:source.catalogHash,epoch:controls.epoch,pid:42,state:'ready',observedAt:at,code:null,initialized:{},pendingInputs:0,operations:{retained:journal.size,maximum:100,bytes:0,maximumBytes:33554432},observedMethods:[]};
   if(name==='projects')return{source:'native',observedAt:at,projects:[{nativeId:definition.project.nativeId,source:'native',name:'Synthetic project',paths:['/synthetic']}]};
   if(name==='frameLimits')return{serviceNodeId:'owner',nativeVersion:source.catalog.version,nativeExecutableHash:source.catalog.nativeExecutableHash,catalogHash:source.catalogHash,epoch:controls.epoch,requestFrameBytes:6291456,receivedFrameBytes:25165824,answerFrameBytes:4194304,managementFrameBytes:33554432};
   if(name==='operation'){
    if(journal.has(args.operationId))return clone(journal.get(args.operationId));
    throw new IvyError('not_found','Exact synthetic owner absence.','not_executed',{kind:'agent_operation_absent',operationId:args.operationId,serviceNodeId:'owner',epoch:controls.epoch});
   }
   if(name==='prevent'){
    if(!journal.has(args.operationId))journal.set(args.operationId,{...operation(args,null,'failed'),epoch:null,requestId:null,reply:null,code:'native_operation_prevented'});
    return clone(journal.get(args.operationId));
   }
   if(name==='read'){
    let reply;
    if(args.method==='thread/read')reply={result:{thread:thread(args.params.threadId)}};
    else{
     const saved=[...journal.values()].filter(v=>v.method==='turn/start').at(-1),turn=saved?.reply?.result?.turn?.id??'delivery-turn-1',answer={id:'delivery-answer',type:'agentMessage',text:'Original delivered reply.\n😀'};
     if(args.method==='thread/items/list')reply={result:{data:[{turnId:turn,item:answer}],nextCursor:null}};
     else if(args.method==='thread/turns/list')reply={result:{data:[{id:turn,status:controls.turnPhase??'inProgress',itemsView:args.params.itemsView,items:args.params.itemsView==='full'?[answer]:[]}],nextCursor:null}};
     else throw Error('Unexpected original delivery read '+args.method);
    }
    const id='delivery-read-'+(++reads);
    return{schemaVersion:1,observationId:id,serviceNodeId:'owner',callerPrincipalId:'chat-service',nativeVersion:source.catalog.version,nativeExecutableHash:source.catalog.nativeExecutableHash,catalogHash:source.catalogHash,epoch:controls.epoch,requestId:id+'-request',observedAt:at,...clone(args),requestHash:hashJson({method:args.method,params:args.params}),reply};
   }
   if(name==='invoke'){
    if(!journal.has(args.operationId)){
     if(args.method==='thread/start'){
      const id='native-main-'+(++starts),reply={result:{thread:thread(id),cwd:'/synthetic/project',model:'fixture-model',modelProvider:'openai',approvalPolicy:'on-request',approvalsReviewer:'user',sandbox:{type:'readOnly'},reasoningEffort:'high'}};
      journal.set(args.operationId,{...operation(args,reply,controls.phase),requestId:'original-request-'+(journal.size+1)});
     }else{
      if(args.method==='thread/resume'){controls.threadStatus='idle';controls.directInput=true;}
      const reply=args.method==='turn/start'?{result:{turn:{id:'delivery-turn-'+(++turns),status:'inProgress',items:[]}}}:args.method==='turn/interrupt'?{result:{}}:{result:{thread:thread(args.params.threadId),cwd:'/synthetic/project',model:'fixture-model',modelProvider:'openai',approvalPolicy:'on-request',approvalsReviewer:'user',sandbox:{type:'readOnly'},reasoningEffort:'high'}};
      journal.set(args.operationId,operation(args,reply));
     }
    }
    if(kind==='cancel-running'&&args.method==='turn/interrupt')controls.turnPhase='interrupted';
    return clone(journal.get(args.operationId));
   }
  }
  throw Error('Unexpected active service RPC '+method+' '+JSON.stringify(p));
 }
 async function request(method,params){
  const event={method,params:clone(params)};events.push(event);assertClaim(method,params,event);
  try{const value=await rpc(method,params);event.result={value:clone(value)};return value;}
  catch(e){event.result={error:error(e)};throw e;}
  finally{if(method==='objects.write')event.records=clone(records);}
 }
 function snapshot(output){
  const documents=observer.prepare('SELECT id,key,name,parent_id,current_revision FROM documents').all(),revisions=observer.prepare('SELECT d.name,d.key,r.id,r.revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id').all(),blobs=observer.prepare('SELECT id,key,name,parent_id,bytes FROM blobs').all();
  for(const d of documents)aliases.set(d.id,'<local:'+d.name+'>');for(const b of blobs)aliases.set(b.id,'<blob:'+b.name+'>');
  for(const event of events)if(event.params.mutationId){const id=event.params.mutationId;if(!mutations.has(id))mutations.set(id,'<mutation:'+mutations.size+'>');aliases.set(id,mutations.get(id));}
  function normalize(v){
   if(typeof v==='string'){for(let n=0;n<8;n++){let next=v;for(const[id,alias]of aliases)next=next.replaceAll(id,alias);if(next===v)break;v=next;}return v;}
   return Array.isArray(v)?v.map(normalize):v&&typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,v])=>[k,normalize(v)])):v;
  }
  const sort=rows=>normalize(rows).sort((a,b)=>Buffer.compare(Buffer.from(canonical(a)),Buffer.from(canonical(b))));
  const value={output:normalize(output),documents:sort(documents.map(d=>({id:d.id,key:d.key,name:d.name,parentId:d.parent_id,revision:d.current_revision}))),revisions:sort(revisions.map(r=>({id:r.id,key:r.key,name:r.name,revision:r.revision,value:JSON.parse(r.value_json)}))),blobs:sort(blobs.map(b=>({id:b.id,key:b.key,name:b.name,parentId:b.parent_id,byteLength:b.bytes.length,contentHash:digest(b.bytes)}))),records:normalize(records),transport:normalize(events)};events.length=0;return value;
 }
 return{root,observer,records,main,request,snapshot,close(){observer.close();rmSync(root,{recursive:true,force:true});}};
}

let scope,serial=0;const sockets=new Map(),pending=new Map();
const server=new WebSocketServer({host:'127.0.0.1',port:0,perMessageDeflate:false,maxPayload:32*1024*1024});await once(server,'listening');
const base=`http://127.0.0.1:${server.address().port}/ivy/`;
const send=(s,v)=>{if(s.readyState===s.OPEN)s.send(JSON.stringify(v));};
function open(spec){
 if(scope)throw Error('Previous isolated case was not closed');
 const env=environment(spec.seed,spec.kind);
 scope={spec,env,observer:env.observer,started:performance.now(),calls:[],handshakes:[],heartbeats:[],notifications:[],bad:[],pending:0,generation:6,reads:0,hold:null,held:false,armReads:0};
 return{url:base,root:env.root};
}
function summary(){return{calls:scope.calls,handshakes:scope.handshakes,heartbeats:scope.heartbeats,notifications:scope.notifications,bad:scope.bad,reads:scope.reads,held:scope.held,readsAfterArm:scope.reads-scope.armReads,pending:scope.pending,elapsedMs:Math.round(performance.now()-scope.started)};}
async function quiet(){let previous='',stable=0;for(let n=0;n<1000;n++){const count=scope.calls.length+':'+scope.observer.prepare('PRAGMA data_version').get().data_version;if(!scope.pending&&count===previous){if(++stable>=20)return;}else stable=0;previous=count;await delay(10);}throw Error('Autonomous service did not settle');}
async function invoke(name,args){
 const[s,session]=[...sockets].find(([,v])=>v.generation===7)??[];if(!s||!session.registry)throw Error('No registered provider');
 const definition=session.registry.namespaces[0].tools.find(t=>t.name===name),id='active-'+(++serial),response=new Promise(done=>pending.set(id,done));
 send(s,{jsonrpc:'2.0',id,method:'provider.invoke',params:{qualifiedName:'chat.'+name,arguments:args,definitionHash:toolDefinitionHash(definition),generation:7,callerPrincipalId:'alice',...(!definition.annotations?.readOnlyHint?{operationId:args.operationId}:{})}});
 const frame=await response;delete frame.id;return frame;
}
async function command(c){
 if(c.action==='begin')return open(c.spec);
 if(c.action==='summary')return summary();
 if(c.action==='failure')return{report:summary(),snapshot:scope.env.snapshot(null),files:['chat-bridge.sqlite','chat-bridge.sqlite-wal','chat-bridge.sqlite-shm'].filter(n=>existsSync(scope.env.root+'/'+n)).map(name=>({name,value:readFileSync(scope.env.root+'/'+name).toString('base64')})),root:scope.env.root};
 if(c.action==='state')return{main:scope.env.main()?.value??null,inputs:scope.env.records.filter(r=>r.object.contractKey==='chat-bridge/input').map(r=>r.content.value),collections:scope.observer.prepare("SELECT r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/collection'").all().map(r=>JSON.parse(r.value_json)),reads:scope.reads};
 if(c.action==='quiet'){await quiet();return summary();}
 if(c.action==='invoke')return invoke(c.name,c.arguments);
 if(c.action==='arm'){
  scope.armReads=scope.reads;scope.held=false;let release;scope.hold={promise:new Promise(done=>release=done),release:()=>release()};return null;
 }
 if(c.action==='notify'){
  const frame={jsonrpc:'2.0',method:'notifications.provider',params:{namespace:'agent',name:'notification',version:'1.0.0',serviceNodeId:'owner',generation:4,payload:{method:c.method??'turn/completed',params:{turn:{id:c.turnId}}}}};
  scope.notifications.push(frame);for(const s of sockets.keys())send(s,frame);return null;
 }
 if(c.action==='release'){const hold=scope.hold;scope.hold=null;hold.release();return null;}
 if(c.action==='end'){
  await quiet();const snapshot=scope.env.snapshot(c.output),report=summary();scope.env.close();scope=null;return{snapshot,report};
 }
 throw Error('Unexpected active fixture command '+c.action);
}
server.on('connection',socket=>{
 const owned=scope,session={generation:++owned.generation,registry:null};sockets.set(socket,session);socket.on('close',()=>sockets.delete(socket));
 socket.on('message',async bytes=>{
  const frame=JSON.parse(bytes.toString());if(!frame.method){pending.get(frame.id)?.(frame);pending.delete(frame.id);return;}
  const{id,method,params}=frame,call={method,params,elapsedMs:Math.round(performance.now()-owned.started)};owned.calls.push(call);owned.pending++;
  try{
   let value;
   if(method==='service.connect'){owned.handshakes.push({method,params});value={generation:session.generation};}
   else if(method==='registry.sync'){session.registry=params;owned.handshakes.push({method,params});value={};}
   else if(method==='notifications.subscribe'){owned.handshakes.push({method,params});value={};}
   else if(method==='service.heartbeat'){owned.heartbeats.push(params);value={ready:params.ready};}
   else{
    if(method==='tools.call'&&params.qualifiedName==='agent.read'&&params.arguments.method==='thread/turns/list'){
     owned.reads++;if(owned.hold&&!owned.held){owned.held=true;await owned.hold.promise;}
    }
    value=await owned.env.request(method,params);
   }
   call.value=value;send(socket,{jsonrpc:'2.0',id,result:value});
  }catch(e){if(!(e instanceof IvyError))owned.bad.push({method,params,error:String(e)});call.error=error(e);send(socket,{jsonrpc:'2.0',id,error:IvyError.from(e).toWire()});}
  finally{owned.pending--;}
 });
});
console.log(JSON.stringify({url:base}));for await(const line of createInterface({input:process.stdin,crlfDelay:Infinity})){
 try{console.log(JSON.stringify({value:await command(JSON.parse(line))}));}catch(e){console.log(JSON.stringify({error:error(e)}));}
}
