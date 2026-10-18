import './chat-store-loader.mjs';
import {mkdtempSync,readFileSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
const {ChatStore}=await import('../../../services/chat-bridge/src/store.ts');
const {ChatAdmission}=await import('../../../services/chat-bridge/src/admission.ts');
const {ChatOperations}=await import('../../../services/chat-bridge/src/operations.ts');
const {ChatOutbox}=await import('../../../services/chat-bridge/src/outbox.ts');
const {mainName}=await import('../../../services/chat-bridge/src/queue.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {hashJson}=await import('../../../packages/contracts/src/canonical.ts');
const {parseOperationId}=await import('../../../packages/contracts/src/operation-id.ts');
const {unpersistedNativePlan}=await import('../../fixtures/native-plan.ts');
const now=1790000000123,at=new Date(now).toISOString(),RealDate=Date;
globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[now]));}static now(){return now;}};
const channel={adapter:'whatsapp',accountId:'account',channelId:'channel'};
const other={...channel,channelId:'foreign'};
const definition={workspaceId:'chat',principalId:'chat-service',rootObjectId:null,project:{serviceNodeId:'owner',namespace:'codex',kind:'project',nativeId:'/synthetic/project'},nativePlan:unpersistedNativePlan(),channels:[{channel,displayName:'Channel'}]};
const pin=(objectId,revision=1)=>({objectId,revision});
const record=(id,key,value,parentId=null,name=id)=>({object:{id,parentId,name,contractKey:'chat-bridge/'+key,effectivelyArchived:false,createdAt:at},revision:{revision:1,contractVersion:'1.3.0',mediaType:'application/json'},content:{encoding:'json',value}});
function answer(name='a',sequence=1,selected=channel,options={}){
 const binding=pin('binding'),result=pin('result-'+name),inputId='input-'+name;
 const input={schemaVersion:1,workspaceId:'chat',definitionHash:new ChatAdmission(definition).definitionHash,
  identity:{channel:structuredClone(selected),senderPrincipalId:'bob',messageId:'message-'+name},operationId:'send-'+name,requestHash:hashJson({name}),binding,sequence,
  payload:{text:'Original input '+name,images:[]},state:'completed',nativeCalls:{resume:null,turn:null,interrupt:null},turnId:'turn-'+name,epoch:'fixture',result,error:null,cancellation:null,admittedAt:at,updatedAt:at,finishedAt:at};
 const immutable={schemaVersion:1,workspaceId:'chat',definitionHash:input.definitionHash,channel:structuredClone(selected),origin:{kind:'native',inputId,binding,result},sequence:sequence+1,partIndex:0,text:'Saved reply '+name,artifacts:[],createdAt:at,...options};
 return [record(inputId,'input',input),record(result.objectId,'result',{schemaVersion:1,inputId,binding,turnId:'turn-'+name,nativeState:'completed',evidence:[pin(inputId)],textParts:[],createdAt:at},inputId),
  record('reply-'+name,'reply',{...immutable,immutableHash:hashJson(immutable),state:'pending',firstOfferedAt:null,confirmedAt:null})];
}
function finalNotice(){
 const immutable={schemaVersion:1,workspaceId:'chat',definitionHash:new ChatAdmission(definition).definitionHash,channel,origin:{kind:'notice',operation:pin('producer-operation'),claim:pin('producer-claim'),producerPrincipalId:'producer',source:pin('source')},sequence:1,partIndex:0,text:'Final notice',artifacts:[],createdAt:at};
 return record('final-notice','reply',{...immutable,immutableHash:hashJson(immutable),state:'pending',firstOfferedAt:null,confirmedAt:null},null,'Chat notice producer-operation');
}
const value=(r,path)=>path.startsWith('data:/')?path.slice(6).split('/').reduce((v,k)=>v?.[k],r.content.value):path.split('.').reduce((v,k)=>v?.[k],r);
function matches(r,p){switch(p.op){case 'and':return p.args.every(p=>matches(r,p));case 'or':return p.args.some(p=>matches(r,p));case 'isNull':return value(r,p.field)===null;case 'eq':return value(r,p.field)===p.value;case 'ne':return value(r,p.field)!==p.value;case 'gt':return value(r,p.field)>p.value;case 'lte':return value(r,p.field)<=p.value;default:throw Error('Unsupported predicate '+p.op);}}
class Client{
 constructor(records){this.records=structuredClone(records);this.versions=new Map(records.map(r=>[r.object.id,new Map([[1,structuredClone(r)]]) ]));this.calls=[];this.fault=null;this.queryCursor=undefined;}
 async request(method,p){
  const call={method,params:structuredClone(p)};
  if(method==='objects.write'){if(parseOperationId(p.mutationId).runtimeEpoch!=='chat')throw Error('Wrong mutation epoch');call.params.mutationId='<scoped>';}
  this.calls.push(call);
  if(method==='system.status')return{runtimeEpoch:'chat'};
  if(method==='objects.query'){
   let selected=this.records.filter(r=>r.object.contractKey===p.contractKey&&(p.includeArchived||!r.object.effectivelyArchived)&&matches(r,p.where));
   for(const sort of [...(p.orderBy??[])].reverse())selected.sort((a,b)=>value(a,sort.field)<value(b,sort.field)?-1:value(a,sort.field)>value(b,sort.field)?1:0);
   const offset=Number(p.cursor??0),items=selected.slice(offset,offset+p.limit).map(r=>pin(r.object.id,r.revision.revision));
   return{items,nextCursor:this.queryCursor===undefined?(offset+p.limit<selected.length?String(offset+p.limit):null):this.queryCursor};
  }
  if(method==='objects.read'){
   const current=this.records.find(r=>r.object.id===p.objectId);if(!current)throw new IvyError('object_missing','Fixture object is missing.');
   const selected=p.revision===undefined?current:this.versions.get(p.objectId)?.get(p.revision);
   if(!selected)throw new IvyError('revision_pruned','Fixture revision was pruned.');return structuredClone(selected);
  }
  if(method!=='objects.write')throw Error('Unexpected RPC '+method);
  const current=this.records.find(r=>r.object.id===p.objectId);if(!current)throw Error('Unknown fixture write');
  if(this.fault?.objectId===p.objectId&&(!this.fault.state||this.fault.state===p.content.value.state)){
   if(this.fault.kind==='contention'&&this.fault.remaining-->0)throw new IvyError('revision_conflict','Fixture revision changed.');
   if(this.fault.kind==='before'){this.fault=null;throw new IvyError('fixture_unavailable','Fixture write was not executed.');}
  }
  if(current.revision.revision!==p.expectedRevision)throw new IvyError('revision_conflict','Fixture revision changed.');
  current.revision.revision++;current.content=structuredClone(p.content);this.versions.get(p.objectId).set(current.revision.revision,structuredClone(current));
  if(this.fault?.kind==='after'&&this.fault.objectId===p.objectId&&(!this.fault.state||this.fault.state===current.content.value.state)){
   this.fault=null;throw new IvyError('outcome_unknown','Fixture reply was lost after the write committed.','unknown');
  }
  return structuredClone(current);
 }
}
const history=(options={})=>({action:'history',...options});
const receive=(name='receive',options={})=>({action:'receive',name,...options});
const ack=(name='ack',offer='receive',ids=['reply-a'],options={})=>({action:'acknowledge',name,offer,ids,...options});
const reopen={action:'reopen'},prune={action:'prune'};
const cases=[];
function add(label,steps,edit=()=>{}){const c={label,definition:structuredClone(definition),records:answer(),steps};edit(c);cases.push(c);}
add('history-receive-subset-ack-replay',[history({limit:3}),receive(),ack(),reopen,ack(),receive(),receive('reoffer'),history()],c=>c.records=[...answer(),...answer('private',3,other),...answer('b',5)]);
add('pruned-revision-original-snapshot',[receive(),prune,ack(),reopen,receive(),ack()]);
add('lost-receive-mark-reply',[{action:'fault',fault:{kind:'after',objectId:'reply-a',state:'outcome_unknown'}},receive(),reopen,receive(),ack()]);
add('lost-ack-mark-reply',[receive(),{action:'fault',fault:{kind:'after',objectId:'reply-a',state:'confirmed'}},ack(),reopen,ack(),receive()]);
add('unapplied-ack-mark-retry',[receive(),{action:'fault',fault:{kind:'before',objectId:'reply-a',state:'confirmed'}},ack(),reopen,ack()]);
add('mark-contention-bound-and-recovery',[{action:'fault',fault:{kind:'contention',objectId:'reply-a',remaining:16}},receive(),ack()]);
add('empty-offer-and-cursor',[receive('empty',{after:9}),receive('empty',{after:9}),history({after:9})],c=>c.records=[]);
add('foreign-caller-and-selection',[receive(),ack('foreign','receive',['reply-a'],{caller:'bob'}),ack('unknown','receive',['unoffered']),ack(),ack('ack','receive',['reply-b']),receive('receive',{limit:1})]);
for(const key of ['channel','expectedBridge'])add('foreign-'+key,[history({bad:key}),receive('bad',{bad:key})]);
for(const [name,patch]of [['text',{text:'Altered'}],['sequence',{sequence:7}],['workspace',{workspaceId:'other'}],['state',{state:'queued'}]]){
 add('reply-'+name,[{action:'reply',id:'reply-a'}],c=>{
  const r=c.records.find(r=>r.object.id===(name==='state'?'input-a':'reply-a'));Object.assign(r.content.value,patch);
 });
}
for(const [name,edit]of [
 ['input-binding',c=>c.records[0].content.value.binding=pin('other')],['result-binding',c=>c.records[1].content.value.binding=pin('other')],
 ['result-turn',c=>c.records[1].content.value.turnId='other'],['result-parent',c=>c.records[1].object.parentId='other'],
 ['reply-channel',c=>{c.records[2].content.value.channel=other;const v=c.records[2].content.value;const{state,firstOfferedAt,confirmedAt,immutableHash,...immutable}=v;v.immutableHash=hashJson(immutable);}]
])add(name,[{action:'reply',id:'reply-a'}],edit);
add('retained-definition-reply',[{action:'reply',id:'reply-a'},history()],c=>{
 for(const r of c.records)if(r.content.value.definitionHash)r.content.value.definitionHash='sha256:'+'a'.repeat(64);
 const v=c.records[2].content.value,{state,firstOfferedAt,confirmedAt,immutableHash,...immutable}=v;v.immutableHash=hashJson(immutable);
});
add('duplicate-merged-sequence',[history()],c=>c.records=[...answer(),...answer('b',2)]);
add('hidden-input-cursor-order',[history({limit:2}),history({after:2,limit:2}),history({after:4,limit:2})],c=>{
 c.records=[...answer(),...answer('b',3),...answer('c',5)];for(const r of c.records)if(r.object.contractKey==='chat-bridge/input')r.content.value.identity.messageId=r.content.value.operationId;
});
add('falsey-nonnull-history-cursor',[{action:'cursor',value:false},history()]);
add('rooted-history-receive',[history(),receive(),ack()],c=>{c.definition.rootObjectId='root';const hash=new ChatAdmission(c.definition).definitionHash;for(const r of c.records){if(r.object.parentId===null)r.object.parentId='root';if(r.content.value.definitionHash)r.content.value.definitionHash=hash;}const v=c.records[2].content.value,{state,firstOfferedAt,confirmedAt,immutableHash,...immutable}=v;v.immutableHash=hashJson(immutable);});
add('final-notice-self-contained',[{action:'reply',id:'final-notice'},history(),receive(),ack('notice-ack','receive',['final-notice'])],c=>c.records=[finalNotice()]);
for(const [name,edit]of [['producer',v=>v.origin.producerPrincipalId=''],['source',v=>v.origin.source.revision=0],['part',v=>v.partIndex=1],['artifact',v=>v.artifacts=[{}]]])add('final-notice-'+name,[{action:'reply',id:'final-notice'}],c=>{const r=finalNotice();edit(r.content.value);const{state,firstOfferedAt,confirmedAt,immutableHash,...immutable}=r.content.value;r.content.value.immutableHash=hashJson(immutable);c.records=[r];});
add('ordered-notice-parts-and-confirmation',[{action:'parts'},{action:'confirm'},{action:'confirm'},reopen,{action:'reply',id:'reply-a'},{action:'parts'}],c=>{
 const first=c.records[2],second=structuredClone(first);second.object.id='reply-a-1';second.object.name='part-one';Object.assign(second.content.value,{sequence:3,partIndex:1,text:'Second part'});const{state,firstOfferedAt,confirmedAt,immutableHash,...immutable}=second.content.value;second.content.value.immutableHash=hashJson(immutable);c.records.push(second);
});
add('misordered-notice-parts',[{action:'parts'}],c=>{c.records[2].content.value.partIndex=1;const{state,firstOfferedAt,confirmedAt,immutableHash,...immutable}=c.records[2].content.value;c.records[2].content.value.immutableHash=hashJson(immutable);});
add('empty-notice-confirmation',[{action:'confirm',empty:true}]);
add('complete-byte-bounded-pages',[receive(),receive('next',{afterOffer:'receive'}),history()],c=>{
 c.compact=true;c.records=[];const artifact={object:pin('artifact'),contentHash:'sha256:'+'1'.repeat(64),byteLength:1,mediaType:'text/plain',label:'\0'.repeat(256)};
 for(let i=0;i<8;i++){const group=answer(String(i),i*2+1,channel,{text:'\0'.repeat(16384),artifacts:Array.from({length:32},()=>artifact)});group[0].content.value.payload.text='\0'.repeat(65536);c.records.push(...group);}
});

const root=mkdtempSync(join(tmpdir(),'ivy-chat-outbox-base-'));const base=new ChatStore({},null,root);base.close();
const database=readFileSync(join(root,'chat-bridge.sqlite')).toString('base64');rmSync(root,{recursive:true,force:true});
async function outcome(action){try{return{value:await action()};}catch(error){const e=IvyError.from(error);return{error:{code:e.code,message:e.message,outcome:e.outcome}};}}
for(const c of cases){
 const path=mkdtempSync(join(tmpdir(),'ivy-chat-outbox-'));writeFileSync(join(path,'chat-bridge.sqlite'),Buffer.from(database,'base64'));
 const client=new Client(c.records),admission=new ChatAdmission(c.definition),requests=new Map();
 let store=new ChatStore(client,c.definition.rootObjectId,path),ops=new ChatOperations(store,admission),outbox=new ChatOutbox(ops),parts=[];
 const operation=async(name,caller='alice')=>ops.find(caller,requests.get(name)?.operationId??`chat:${now}:${name}`);
 const offer=async(name)=>{const op=await operation(name);return store.named('chat-bridge/offer','Chat offer '+op.pin.objectId);};
 const normalize=(value,ids)=>{
  if(typeof value==='string')return ids.get(value)??value;
  if(Array.isArray(value))return value.map(v=>normalize(v,ids));
  if(value&&typeof value==='object'){
   const result=Object.fromEntries(Object.entries(value).map(([k,v])=>[k,normalize(v,ids)]));
   if(result.request&&result.requestHash)result.requestHash=hashJson(result.request);return result;
  }return value;
 };
 async function snapshot(output){
  const documents=[],ids=new Map();
  for(const [name,request]of requests){
   const op=await ops.find(request.expectedBridge.callerPrincipalId,request.operationId);if(!op)continue;
   ids.set(op.pin.objectId,'operation:'+name);documents.push({name,kind:'operation',object:op.pin,data:op.value});
   for(const [kind,prefix]of [['offer','Chat offer '],['acknowledgement','Chat acknowledgement ']]){
    const doc=await store.named('chat-bridge/'+kind,prefix+op.pin.objectId);if(doc){ids.set(doc.pin.objectId,kind+':'+name);documents.push({name,kind,object:doc.pin,data:doc.value});}
   }
  }
  const data=normalize({output,documents,records:client.records,calls:client.calls},ids);
  return{hash:hashJson(data),...(!c.compact?{snapshot:data}:{})};
 }
 try{
  c.expected=[];
  for(const step of c.steps){
   const output=await outcome(async()=>{
    const caller=step.caller??'alice';
    switch(step.action){
     case 'reopen':store.close();store=new ChatStore(client,c.definition.rootObjectId,path);ops=new ChatOperations(store,admission);outbox=new ChatOutbox(ops);return null;
     case 'fault':client.fault=structuredClone(step.fault);return null;
     case 'cursor':client.queryCursor=step.value;return null;
     case 'prune':for(const r of client.records)client.versions.set(r.object.id,new Map([[r.revision.revision,structuredClone(r)]]));return null;
     case 'reply':return outbox.reply(step.id,channel).then(d=>({object:d.pin,data:d.value}));
     case 'parts':parts=await outbox.noticeParts(await outbox.reply('reply-a',channel));return parts.map(d=>({object:d.pin,data:d.value}));
     case 'confirm':await outbox.confirmNotice(step.empty?[]:parts);return null;
     case 'history':{
      const request={expectedBridge:admission.expected(caller),channel:step.bad==='channel'?other:channel,afterSequence:step.after??0,limit:step.limit??8};
      if(step.bad==='expectedBridge')request.expectedBridge.callerPrincipalId='foreign';return outbox.history(caller,request);
     }
     case 'receive':{
      const after=step.afterOffer?(await offer(step.afterOffer)).value.throughSequence:(step.after??0);
      const request={action:'receive',operationId:`chat:${now}:${step.name}`,expectedBridge:admission.expected(caller),channel:step.bad==='channel'?other:channel,afterSequence:after,limit:step.limit??8};
      if(step.bad==='expectedBridge')request.expectedBridge.callerPrincipalId='foreign';requests.set(step.name,request);return outbox.receive(caller,request);
     }
     case 'acknowledge':{
      const request={action:'acknowledge',operationId:`chat:${now}:${step.name}`,expectedBridge:admission.expected(caller),offerId:(await offer(step.offer)).pin.objectId,replyIds:step.ids};requests.set(step.name,request);return outbox.acknowledge(caller,request);
     }
     default:throw Error('Bad step '+step.action);
    }
   });
   c.expected.push(await snapshot(output));
  }
 }finally{store.close();rmSync(path,{recursive:true,force:true});}
}
process.stdout.write(JSON.stringify({now,definition,channel,database,cases}));
