import './chat-domain-loader.mjs';
import {mkdtempSync,readFileSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {DatabaseSync} from 'node:sqlite';
import {registerHooks} from 'node:module';
// Publisher imports this name only as a TypeScript annotation. Its concrete
// collector is deliberately outside this source-boundary differential fixture.
registerHooks({resolve(s,c,n){if(c.parentURL?.endsWith('/publisher.ts')&&s==='./collector.js')return{url:'data:text/javascript,export const ChatCollector=undefined;',shortCircuit:true};return n(s,c);}});
const {ChatStore}=await import('../../../services/chat-bridge/src/store.ts');
const {ChatAdmission}=await import('../../../services/chat-bridge/src/admission.ts');
const {ChatOperations}=await import('../../../services/chat-bridge/src/operations.ts');
const {ChatQueue,mainName}=await import('../../../services/chat-bridge/src/queue.ts');
const {ChatNotices}=await import('../../../services/chat-bridge/src/notices.ts');
const {ChatPublisher}=await import('../../../services/chat-bridge/src/publisher.ts');
const {ChatOutbox}=await import('../../../services/chat-bridge/src/outbox.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {hashJson,digest,canonical}=await import('../../../packages/contracts/src/canonical.ts');
const {parseOperationId}=await import('../../../packages/contracts/src/operation-id.ts');
const {unpersistedNativePlan}=await import('../../fixtures/native-plan.ts');
const now=1790000000123,at=new Date(now).toISOString(),RealDate=Date;
globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[now]));}static now(){return now;}};
const channel={adapter:'whatsapp',accountId:'account',channelId:'channel'};
const definition={workspaceId:'chat',principalId:'chat-service',rootObjectId:null,project:{serviceNodeId:'owner',namespace:'codex',kind:'project',nativeId:'/synthetic/project'},nativePlan:unpersistedNativePlan(),channels:[{channel,displayName:'Channel'}]};
const admission=new ChatAdmission(definition);
const result=async(fn)=>{try{return{value:(await fn())??null};}catch(error){const e=IvyError.from(error);return{error:{code:e.code,message:e.message,outcome:e.outcome}};}};
const record=(id,key,value,parentId=null,name=id)=>({object:{id,parentId,name,contractKey:key,effectivelyArchived:false,createdAt:at},revision:{revision:1,contractVersion:'1.3.0',mediaType:'application/json'},content:{encoding:'json',value}});
const pointer=(r,path)=>path.startsWith('data:/')?path.slice(6).split('/').reduce((v,k)=>v?.[k],r.content.value):path.split('.').reduce((v,k)=>v?.[k],r);
function matches(r,p){switch(p.op){case'and':return p.args.every(p=>matches(r,p));case'or':return p.args.some(p=>matches(r,p));case'isNull':return pointer(r,p.field)===null;case'eq':return pointer(r,p.field)===p.value;case'ne':return pointer(r,p.field)!==p.value;case'gt':return pointer(r,p.field)>p.value;case'lte':return pointer(r,p.field)<=p.value;default:throw Error('Unexpected predicate '+p.op);}}
class Client{
 constructor(records=[]){this.records=structuredClone(records);this.versions=new Map(this.records.map(r=>[r.object.id,new Map([[r.revision.revision,structuredClone(r)]])]));this.calls=[];this.fault=null;}
 async request(method,p){
  if(method==='objects.write'&&parseOperationId(p.mutationId).runtimeEpoch!=='chat')throw Error('Wrong mutation epoch.');
  this.calls.push({method,params:structuredClone(p)});
  if(method==='system.status')return{runtimeEpoch:'chat'};
  if(method==='objects.query'){
   let selected=this.records.filter(r=>r.object.contractKey===p.contractKey&&(p.includeArchived||!r.object.effectivelyArchived)&&matches(r,p.where));
   for(const sort of [...(p.orderBy??[])].reverse())selected.sort((a,b)=>pointer(a,sort.field)<pointer(b,sort.field)?-1:pointer(a,sort.field)>pointer(b,sort.field)?1:0);
   const offset=Number(p.cursor??0);return{items:selected.slice(offset,offset+p.limit).map(r=>({objectId:r.object.id,revision:r.revision.revision})),nextCursor:offset+p.limit<selected.length?String(offset+p.limit):null};
  }
  if(method==='objects.read'){
   const current=this.records.find(r=>r.object.id===p.objectId);if(!current)throw new IvyError('object_missing','Fixture object is missing.');
   const selected=p.revision===undefined?current:this.versions.get(p.objectId)?.get(p.revision);if(!selected)throw new IvyError('revision_pruned','Fixture revision was pruned.');return structuredClone(selected);
  }
  if(method!=='objects.write')throw Error('Unexpected RPC '+method);
  const applies=this.fault&&(this.fault.key===p.create?.contractKey||this.fault.objectId===p.objectId);
  if(applies&&this.fault.kind==='before'){this.fault=null;throw new IvyError('fixture_unavailable','Fixture write was not executed.');}
  if(applies&&this.fault.kind==='contention'&&this.fault.remaining-->0)throw new IvyError('revision_conflict','Fixture revision changed.');
  let saved;
  if(p.create){
   if(this.records.some(r=>r.object.contractKey===p.create.contractKey&&r.object.parentId===p.create.parentId&&r.object.name===p.create.name))throw new IvyError('revision_conflict','Fixture name already exists.');
   saved=record('publication-record-'+(this.records.length+1),p.create.contractKey,structuredClone(p.content.value),p.create.parentId,p.create.name);saved.revision.contractVersion=p.contractVersion;this.records.push(saved);this.versions.set(saved.object.id,new Map());
  }else{
   saved=this.records.find(r=>r.object.id===p.objectId);if(!saved)throw new IvyError('object_missing','Fixture object is missing.');
   if(saved.revision.revision!==p.expectedRevision)throw new IvyError('revision_conflict','Fixture revision changed.');saved.revision.revision++;saved.content=structuredClone(p.content);
  }
  this.versions.get(saved.object.id).set(saved.revision.revision,structuredClone(saved));
  if(applies&&this.fault.kind==='after'){this.fault=null;throw new IvyError('outcome_unknown','Fixture reply was lost after the write committed.','unknown');}
  return structuredClone(saved);
 }
 patch(id,patch){const r=this.records.find(r=>r.object.id===id);r.revision.revision++;Object.assign(r.content.value,structuredClone(patch));this.versions.get(id).set(r.revision.revision,structuredClone(r));}
}
// Long strings retain full UTF16 and UTF8 identities in every snapshot; the
// initial fixture records keep the complete original bytes for actual reading.
function compact(v){
 if(typeof v==='string'&&v.length>512)return{utf16Length:v.length,utf16Hash:digest(Buffer.from(v,'utf16le')),utf8Length:Buffer.byteLength(v),utf8Hash:digest(Buffer.from(v))};
 if(Array.isArray(v))return v.map(compact);if(v&&typeof v==='object')return Object.fromEntries(Object.entries(v).map(([k,v])=>[k,compact(v)]));return v;
}
const seeds={};
async function seed(name,caller='bob',text='Original Main answer',following=false){
 const root=mkdtempSync(join(tmpdir(),'ivy-publication-seed-')),client=new Client([record('source','domain/source',{})]);
 let store=new ChatStore(client,null,root);const ops=new ChatOperations(store,admission),queue=new ChatQueue(ops);
 try{
  const binding=await store.write('chat-bridge/binding',{schemaVersion:1,workspaceId:'chat',definitionHash:admission.definitionHash,project:definition.project,primary:{...definition.project,kind:'thread',nativeId:'fixture-thread'},nativePlan:definition.nativePlan,createdAt:at,operationId:'main-creation',predecessor:null},'seed-binding',{create:{parentId:null,name:'fixture-binding'}});
  const mainPin=await store.write('chat-bridge/main',{schemaVersion:1,definition,definitionHash:admission.definitionHash,binding,conversation:{objectId:'conversation',revision:1},pendingAction:null,publication:null,queue:[],nextSequence:1,updatedAt:at},'seed-main',{create:{parentId:null,name:mainName(definition)}});
  if(caller==='bob')await queue.send(caller,{action:'send',operationId:`chat:${now}:original`,expectedBridge:admission.expected(caller),channel,messageId:'original',expectedBinding:binding,payload:{text:'Original input',images:[]}});
  else{
   const main={admission,operations:ops,store,queue,native:{async verifyOwner(){}},async ensure(){},async recoverPending(){await new ChatNotices(main).recoverPending();}};
   await new ChatNotices(main).notify(caller,{action:'notify',operationId:`chat:${now}:original`,source:{objectId:'source',revision:1},text:'Original service notice'});
  }
  const inputId=(await queue.main()).value.queue[0].inputId;client.patch(inputId,{state:'running',turnId:'fixture-turn',epoch:'fixture'});
  if(following)await queue.send('bob',{action:'send',operationId:`chat:${now}:following`,expectedBridge:admission.expected('bob'),channel,messageId:'following',expectedBinding:binding,payload:{text:'Following input',images:[]}});
  const textRecord=record('text','chat-bridge/result-text',text,inputId);textRecord.content.encoding='text';textRecord.revision={revision:1,contractVersion:'1.0.0',mediaType:'text/plain',byteLength:Buffer.byteLength(text),contentHash:digest(Buffer.from(text))};client.records.push(textRecord);client.versions.set('text',new Map([[1,structuredClone(textRecord)]]));
  const artifact={object:{objectId:'text',revision:1},mediaType:'text/plain',byteLength:Buffer.byteLength(text),contentHash:digest(Buffer.from(text)),label:'Original result text'};
  const resultPin=await store.write('chat-bridge/result',{schemaVersion:1,inputId,binding,turnId:'fixture-turn',nativeState:'completed',evidence:[{objectId:'original-evidence',revision:1}],textParts:[artifact],createdAt:at},'seed-result',{create:{parentId:inputId,name:'Fixture result'}});
  const collectionPin=await store.write('chat-bridge/collection',{schemaVersion:1,inputId,binding,turnId:'fixture-turn',state:'complete',result:resultPin,createdAt:at,updatedAt:at},'seed-collection',{create:{parentId:inputId,name:'Chat original turn collection'}});
  store.close();store=null;
  const db=new DatabaseSync(join(root,'chat-bridge.sqlite'));db.exec(`CREATE TABLE publication_test_writes(key TEXT,value_json TEXT);
   CREATE TRIGGER publication_test_revision AFTER INSERT ON revisions BEGIN INSERT INTO publication_test_writes SELECT d.key,NEW.value_json FROM documents d WHERE d.id=NEW.id; END;`);db.close();
  seeds[name]={database:readFileSync(join(root,'chat-bridge.sqlite')).toString('base64'),records:client.records,inputId,mainPin,resultPin,collectionPin,caller};
 }finally{store?.close();rmSync(root,{recursive:true,force:true});}
}
await seed('send');await seed('notice','producer','  [Other] Main answer');await seed('taskboard','Feather.task-board','Unattributed Main answer');
await seed('long','bob','𐐨a\n'.repeat(90000),true);
const step={action:'step'},history={action:'history'},reopen={action:'reopen'};
const control=value=>({action:'control',value}),patch=(target,value)=>({action:'patch',target,value});
const rounds=n=>Array.from({length:n},()=>step),cases=[];
const add=(label,steps,seed='send',crash=null)=>cases.push({label,seed,steps,crash});
add('reserve-terminal-batch-release',[history,step,history,step,history,step,history,step,history,step]);
add('bounded-17-part-head-holds-following',[step,history,step,history,step,history,reopen,step,history,step,history,step,history,step],'long');
add('notice-first-reply-receipt',[...rounds(4),history,step],'notice');add('taskboard-attribution',[...rounds(4),history],'taskboard');
for(const state of ['failed','interrupted'])for(const cancelled of [false,true])add('terminal-'+state+'-'+cancelled,[patch('result',{nativeState:state}),...(cancelled?[patch('input',{cancellation:{operationId:'cancel',callerPrincipalId:'bob',requestedAt:at,reason:'Original cancellation'}})]:[]),...rounds(4),history]);
add('empty-result-fallback',[patch('result',{textParts:[]}),...rounds(4),history]);
add('no-head',[control({noHead:true}),step,control({noHead:false}),...rounds(4)]);
add('unstarted-head',[patch('input',{turnId:null}),step,patch('input',{turnId:'fixture-turn'}),...rounds(4)]);
for(const flag of ['findMissing','findIncomplete','collectMissing','collectIncomplete','collectChanged'])add(flag,[control({[flag]:true}),step,control({[flag]:false}),...rounds(4)]);
for(const method of ['recover','find','collect'])for(const code of ['fixture_unavailable','revision_conflict','mutation_conflict'])add(method+'-'+code,[control({[method+'Error']:code}),step,control({[method+'Error']:null}),...rounds(4)]);
for(const atVerify of [1,2,3,5,11])add('owner-refusal-'+atVerify,[control({refuseAt:atVerify}),...rounds(6),control({refuseAt:0}),...rounds(7)],atVerify>4?'long':'send');
for(const kind of ['before','after','contention'])add('reply-write-'+kind,[step,step,{action:'fault',value:{kind,key:'chat-bridge/reply',remaining:1}},step,reopen,...rounds(4)]);
for(const [field,value]of [['binding',{objectId:'other',revision:1}],['turnId','other'],['finishedAt','2026-01-01T00:00:00.000Z']])add('input-original-'+field,[step,patch('input',{[field]:value}),step]);
for(const [field,value]of [['state','failed'],['updatedAt','2026-01-01T00:00:00.000Z'],['error',{code:'other',outcome:'unknown',detail:'Other terminal'}],['result',{objectId:'other',revision:1}]])add('terminal-refusal-'+field,[step,step,patch('input',{[field]:value}),step]);
add('collection-revision-cache-invalidated',[step,patch('collection',{}),step]);
add('collection-result-cache-invalidated',[step,patch('collection',{result:{objectId:'other',revision:1}}),step]);
add('cache-retains-immutable-material',[step,{action:'remove-text'},step,step,step]);
add('reopen-rereads-original-text',[step,{action:'remove-text'},reopen,step]);
add('safe-sequence-exhaustion',[patch('main',{nextSequence:Number.MAX_SAFE_INTEGER}),step]);
add('pending-main-action',[patch('main',{pendingAction:{objectId:'pending',revision:1}}),step]);
add('changed-head',[step,patch('main',{queue:[]}),step]);
for(const field of ['collection','result','partCount','createdAt'])add('claimed-'+field,[step,{action:'patch-claim',field},step]);
add('notice-first-reply-channel-refusal',[step,step,step,patch('reply',{channel:{...channel,channelId:'other'}}),step],'notice');
add('notice-first-reply-origin-refusal',[step,step,step,{action:'patch-reply-origin'},step],'notice');
for(const barrier of ['reserve','terminal','reply-0','progress','release'])add('original-crash-'+barrier,[history,...rounds(5),history],'send',barrier);
for(const barrier of ['reply-7','progress','reply-8'])add('original-long-crash-'+barrier,[history,...rounds(7),history],'long',barrier);
for(const barrier of ['notice-finish','release'])add('original-notice-crash-'+barrier,[...rounds(5),history],'notice',barrier);

function environment(store,client,seed,control){
 const ops=new ChatOperations(store,admission),queue=new ChatQueue(ops),trace=(method,params={})=>client.calls.push({method,params});
 const fail=method=>{const code=control[method+'Error'];if(code)throw new IvyError(code,'Fixture '+method+' failed.');};
 const main={store,operations:ops,queue,async recoverPending(){trace('main.recoverPending');fail('recover');},native:{async verifyOwner(){trace('native.verifyOwner');control.verifyCount=(control.verifyCount??0)+1;if(control.refuseAt===control.verifyCount)throw new IvyError('fixture_owner_refused','The original native owner is unavailable.');}}};
 // Only the source/authority boundary is synthetic. Publisher, persistence,
 // operation receipts, text validation and history are the production original.
 const collector={main,delivery:{async head(){trace('delivery.head');if(control.noHead)return null;const ticket=(await queue.main()).value.queue[0];return ticket?{input:await store.read('chat-bridge/input',ticket.inputId)}:null;}},
  async find(inputId){trace('collector.find',{inputId});fail('find');if(control.findMissing)return null;const d=await store.named('chat-bridge/collection','Chat original turn collection',inputId);if(d&&control.findIncomplete)d.value.state='reading';return d;},
  async step(inputId){trace('collector.collect',{inputId});fail('collect');if(control.collectMissing)return null;const d=await store.named('chat-bridge/collection','Chat original turn collection',inputId);if(d&&control.collectIncomplete)d.value.state='reading';if(d&&control.collectChanged)d.pin.revision++;return d;}
 };
 return{ops,queue,publisher:new ChatPublisher(collector)};
}
function rows(root,sql,...args){const db=new DatabaseSync(join(root,'chat-bridge.sqlite'));try{return db.prepare(sql).all(...args);}finally{db.close();}}
async function execute(c){
 const seed=seeds[c.seed],root=mkdtempSync(join(tmpdir(),'ivy-publication-')),client=new Client(seed.records),control={};writeFileSync(join(root,'chat-bridge.sqlite'),Buffer.from(seed.database,'base64'));
 let store=new ChatStore(client,null,root),env=environment(store,client,seed,control),lastWrite=0;const mutations=new Map();
 const snapshot=output=>{
  const local=rows(root,'SELECT d.id,d.key,d.current_revision AS revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision ORDER BY d.id').map(r=>({id:r.id,key:r.key,revision:r.revision,value:JSON.parse(r.value_json)}));
  const writes=rows(root,'SELECT rowid,key,value_json FROM publication_test_writes WHERE rowid>? ORDER BY rowid',lastWrite);lastWrite=writes.at(-1)?.rowid??lastWrite;
  const calls=[],batch=[];const flush=()=>{batch.sort((a,b)=>canonical(a)<canonical(b)?-1:canonical(a)>canonical(b)?1:0);calls.push(...batch);batch.length=0;};
  for(let call of client.calls){if(['objects.read','objects.query'].includes(call.method)){batch.push(call);continue;}flush();if(call.method==='objects.write'){const id=call.params.mutationId;if(!mutations.has(id))mutations.set(id,'<mutation:'+mutations.size+'>');call={...call,params:{...call.params,mutationId:mutations.get(id)}};}calls.push(call);}flush();
  const answer=compact({output,local,records:client.records,writes:writes.map(r=>({key:r.key,value:JSON.parse(r.value_json)})),calls});client.calls=[];return answer;
 };
 try{
  if(c.crash){
   const original=store.write.bind(store);let stopped=false;
   store.write=async(key,value,mutation,destination)=>{
    const pin=await original(key,value,mutation,destination);
    const stage=key==='chat-bridge/main'?(value.publication?(value.publication.publishedParts?'progress':'reserve'):'release'):key==='chat-bridge/input'?'terminal':key==='chat-bridge/reply'?'reply-'+value.partIndex:key==='chat-bridge/operation'&&value.phase==='succeeded'?'notice-finish':null;
    if(stage===c.crash&&!stopped){stopped=true;throw new IvyError('outcome_unknown','Fixture reply was lost after '+stage+'.','unknown');}return pin;
   };
   for(let i=0;i<10&&!stopped;i++)c.interrupted=await result(()=>env.publisher.step());if(!stopped)throw Error('Missing original crash barrier '+c.label+': '+JSON.stringify(c.interrupted));
   c.scopeCacheWarm=client.calls.some(c=>c.method==='system.status'||c.method==='objects.write');store.close();c.database=readFileSync(join(root,'chat-bridge.sqlite')).toString('base64');c.records=structuredClone(client.records);store=new ChatStore(client,null,root);env=environment(store,client,seed,control);
   lastWrite=rows(root,'SELECT COALESCE(MAX(rowid),0) AS n FROM publication_test_writes')[0].n;
  }
  c.initialWrite=lastWrite;client.calls=[];c.expected=[];
  for(const s of c.steps){
   const output=await result(async()=>{switch(s.action){
    case'step':return env.publisher.step();
    case'history':return new ChatOutbox(env.ops).history('viewer',{expectedBridge:admission.expected('viewer'),channel,afterSequence:0,limit:8});
    case'control':Object.assign(control,s.value);return;
    case'fault':client.fault=structuredClone(s.value);return;
    case'reopen':store.close();store=new ChatStore(client,null,root);env=environment(store,client,seed,control);return;
    case'remove-text':client.records=client.records.filter(r=>r.object.id!=='text');return;
    case'patch':{
     if(['input','reply'].includes(s.target)){const id=s.target==='input'?seed.inputId:client.records.find(r=>r.object.contractKey==='chat-bridge/reply').object.id;client.patch(id,s.value);return;}
     const p=s.target==='main'?seed.mainPin:s.target==='result'?seed.resultPin:seed.collectionPin,key='chat-bridge/'+s.target,d=await store.read(key,p.objectId,s.target==='main'?null:seed.inputId);
     const written=await store.write(key,{...d.value,...s.value},'fixture-update',{objectId:d.pin.objectId,expectedRevision:d.pin.revision});
     if(s.target==='result'){const collection=await store.read('chat-bridge/collection',seed.collectionPin.objectId,seed.inputId);await store.write('chat-bridge/collection',{...collection.value,result:written},'fixture-update',{objectId:collection.pin.objectId,expectedRevision:collection.pin.revision});}return;
    }
    case'patch-claim':{const d=await store.read('chat-bridge/main',seed.mainPin.objectId),claim={...d.value.publication};claim[s.field]=s.field==='partCount'?2:s.field==='createdAt'?'2026-01-01T00:00:00.000Z':{objectId:'other',revision:1};await store.write('chat-bridge/main',{...d.value,publication:claim,nextSequence:100},'fixture-update',{objectId:d.pin.objectId,expectedRevision:d.pin.revision});return;}
    case'patch-reply-origin':{const r=client.records.find(r=>r.object.contractKey==='chat-bridge/reply');client.patch(r.object.id,{origin:{...r.content.value.origin,inputId:'other'}});return;}
    default:throw Error('Unknown step '+s.action);
   }});c.expected.push(snapshot(output));
  }
 }finally{store.close();rmSync(root,{recursive:true,force:true});}
}
for(const c of cases)await execute(c);
for(const c of cases.slice(0,9))if(c.expected.some(e=>e.output.error))throw Error('Expected successful publication path '+c.label+': '+JSON.stringify(c.expected.map(e=>e.output)));
for(const c of cases.filter(c=>c.label.startsWith('terminal-')&&!c.label.startsWith('terminal-refusal-'))){const input=c.expected.at(-1).records.find(r=>r.object.id===seeds[c.seed].inputId).content.value;const expected=c.label==='terminal-interrupted-true'?'cancelled':'failed';if(input.state!==expected)throw Error('Terminal fixture did not publish '+expected);}
const baseline=cases[0].expected;for(const index of [0,2,4,6])if(baseline[index].output.value.entries.some(e=>e.data.origin))throw Error('Partial publication became visible.');
if(!baseline[8].output.value.entries.some(e=>e.data.origin?.kind==='native'))throw Error('Released publication was not visible.');
const races=[];
for(const seedName of ['send','notice','long']){
 const seed=seeds[seedName],root=mkdtempSync(join(tmpdir(),'ivy-publication-race-')),client=new Client(seed.records);writeFileSync(join(root,'chat-bridge.sqlite'),Buffer.from(seed.database,'base64'));
 const stores=[new ChatStore(client,null,root),new ChatStore(client,null,root)],envs=stores.map(s=>environment(s,client,seed,{}));
 try{
  for(let n=0;n<10;n++){
   const outcomes=await Promise.all(envs.map(e=>result(()=>e.publisher.step())));if(outcomes.some(o=>o.error))throw Error('Original race failed '+JSON.stringify(outcomes));
  }
  const main=(await envs[0].queue.main()).value,input=client.records.find(r=>r.object.id===seed.inputId).content.value,replies=client.records.filter(r=>r.object.contractKey==='chat-bridge/reply').map(r=>r.content.value).sort((a,b)=>a.sequence-b.sequence),op=await envs[0].ops.find(seed.caller,`chat:${now}:original`);
  races.push({seed:seedName,expected:compact({main,input,replies,operation:op.value})});
 }finally{for(const store of stores)store.close();rmSync(root,{recursive:true,force:true});}
}
process.stdout.write(JSON.stringify({now,definition,channel,seeds,cases,races}));
