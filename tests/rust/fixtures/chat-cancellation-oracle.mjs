import {Client as Records,compact} from './chat-collector-oracle.mjs';
import {mkdtempSync,readFileSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,basename} from 'node:path';
import {DatabaseSync} from 'node:sqlite';
import {buildInstructions} from '../../../tools/build/build-instructions.mjs';
const {ChatStore}=await import('../../../services/chat-bridge/src/store.ts');
const {ChatAdmission}=await import('../../../services/chat-bridge/src/admission.ts');
const {ChatOperations}=await import('../../../services/chat-bridge/src/operations.ts');
const {ChatQueue,mainName}=await import('../../../services/chat-bridge/src/queue.ts');
const {ChatNotices}=await import('../../../services/chat-bridge/src/notices.ts');
const {ChatCancellation}=await import('../../../services/chat-bridge/src/cancellation.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {canonical,hashJson,digest}=await import('../../../packages/contracts/src/canonical.ts');
const {unpersistedNativePlan}=await import('../../fixtures/native-plan.ts');
const now=1790000000123,at=new Date(now).toISOString(),channel={adapter:'whatsapp',accountId:'account',channelId:'channel'};
const definition={workspaceId:'chat',principalId:'chat-service',rootObjectId:null,project:{serviceNodeId:'owner',namespace:'codex',kind:'project',nativeId:'/synthetic/project'},nativePlan:unpersistedNativePlan(),channels:[{channel,displayName:'Channel'}]},admission=new ChatAdmission(definition);
const clone=v=>structuredClone(v),error=e=>{e=IvyError.from(e);return{code:e.code,message:e.message,outcome:e.outcome};},result=async fn=>{try{return{value:(await fn())??null};}catch(e){return{error:error(e)};}};
const pointer=(record,path)=>path.startsWith('data:/')?path.slice(6).split('/').reduce((v,k)=>v?.[k],record.content.value):path.split('.').reduce((v,k)=>v?.[k],record);
function matches(r,p){switch(p.op){case'and':return p.args.every(p=>matches(r,p));case'or':return p.args.some(p=>matches(r,p));case'isNull':return pointer(r,p.field)===null;case'eq':return pointer(r,p.field)===p.value;default:throw Error('Unsupported cancellation predicate '+p.op);}}
class Client extends Records{
 constructor(records=[],history=null){super(records);if(history)this.versions=new Map(history.map(([id,rows])=>[id,new Map(clone(rows))]));}
 async request(method,p){if(method!=='objects.query')return super.request(method,p);this.calls.push({method,params:clone(p)});const rows=this.records.filter(r=>r.object.contractKey===p.contractKey&&matches(r,p.where));return{items:rows.slice(0,p.limit).map(r=>({objectId:r.object.id,revision:r.revision.revision})),nextCursor:rows.length>p.limit?'more':null};}
 history(){return [...this.versions].map(([id,rows])=>[id,[...rows]]);}
 patch(id,patch){const r=this.records.find(r=>r.object.id===id);Object.assign(r.content.value,clone(patch));r.revision.revision++;this.versions.get(id).set(r.revision.revision,clone(r));}
}
const assetRoot=mkdtempSync(join(tmpdir(),'ivy-cancel-assets-'));let instructionAsset;
try{const files=buildInstructions({root:process.cwd(),outputRoot:assetRoot,component:'chat-bridge'});instructionAsset=readFileSync(files.find(p=>basename(p)==='chat-notice-prompt.json')).toString('base64');}finally{rmSync(assetRoot,{recursive:true,force:true});}
const seeds={};
for(const kind of ['send','notice']){
 const root=mkdtempSync(join(tmpdir(),'ivy-cancel-seed-')),client=new Client(kind==='notice'?[{object:{id:'source',parentId:null,contractKey:'domain/source',name:'source',effectivelyArchived:false},revision:{revision:1,contractVersion:'1.0.0',mediaType:'application/json'},content:{encoding:'json',value:{}}}]:[]),store=new ChatStore(client,null,root),ops=new ChatOperations(store,admission),queue=new ChatQueue(ops);
 try{
  store.setRuntimeEpoch('chat');
  const binding=await store.write('chat-bridge/binding',{schemaVersion:1,workspaceId:'chat',definitionHash:admission.definitionHash,project:definition.project,primary:{...definition.project,kind:'thread',nativeId:'original-thread'},nativePlan:definition.nativePlan,createdAt:at,operationId:'original-binding',predecessor:null},'seed',{create:{parentId:null,name:'Original binding'}});
  await store.write('chat-bridge/main',{schemaVersion:1,definition,definitionHash:admission.definitionHash,binding,conversation:{objectId:'conversation',revision:1},pendingAction:null,publication:null,queue:[],nextSequence:1,updatedAt:at},'seed',{create:{parentId:null,name:mainName(definition)}});
  await queue.send('alice',{action:'send',operationId:`chat:${now}:head`,expectedBridge:admission.expected('alice'),channel,messageId:'head',expectedBinding:binding,payload:{text:'Head input',images:[]}});
  if(kind==='send')await queue.send('alice',{action:'send',operationId:`chat:${now}:tail`,expectedBridge:admission.expected('alice'),channel,messageId:'tail',expectedBinding:binding,payload:{text:'Tail input',images:[]}});
  else{const main={store,admission,operations:ops,queue,native:{async verifyOwner(){}},async ensure(){},async recoverPending(){await new ChatNotices(main).recoverPending();}};await new ChatNotices(main).notify('producer',{action:'notify',operationId:`chat:${now}:notice`,source:{objectId:'source',revision:1},text:'Original notice'});}
  const tickets=(await queue.main()).value.queue,inputs={head:tickets[0].inputId,tail:tickets[1].inputId};store.close();
  const db=new DatabaseSync(join(root,'chat-bridge.sqlite'));db.exec(`CREATE TABLE collector_test_writes(key TEXT,id TEXT,revision INTEGER,value_json TEXT);CREATE TRIGGER cancellation_test_revision AFTER INSERT ON revisions BEGIN INSERT INTO collector_test_writes SELECT key,NEW.id,NEW.revision,NEW.value_json FROM documents WHERE id=NEW.id;END;`);db.close();
  seeds[kind]={database:readFileSync(join(root,'chat-bridge.sqlite')).toString('base64'),records:client.records,history:client.history(),inputs,binding};
 }finally{try{store.close();}catch{}rmSync(root,{recursive:true,force:true});}
}
const cases=[],cancel=(extra={})=>({action:'cancel',name:'c',target:'tail',...extra}),reopen={action:'reopen'},recover={action:'recover'},intent={action:'recover-intent',target:'tail'},patch=(target,value)=>({action:'patch-input',target,value});
const add=(label,steps,extra={})=>cases.push({label,seed:'send',steps,...extra});
add('queued-tail-head-and-original-receipt',[cancel(),intent,reopen,cancel({replay:true}),cancel({target:'head',name:'head'}),recover,cancel({replay:true})]);
add('notice-cancellation-finishes-producer',[cancel(),intent,reopen,cancel({replay:true})],{seed:'notice'});
for(const value of [{turnId:'original-turn',epoch:'epoch',state:'running'},{nativeCalls:{resume:{objectId:'resume',revision:1},turn:null,interrupt:null}},{nativeCalls:{resume:null,turn:{objectId:'turn',revision:1},interrupt:null}}])add('retained-native-work-'+cases.length,[patch('tail',value),cancel(),intent,reopen,cancel({replay:true})]);
add('caller-authorization-before-content',[cancel({caller:'mallory',badBridge:true})]);
add('missing-bridge-before-cancel-shape',[cancel({omitBridge:true})]);
add('invalid-shape-after-caller-check',[cancel({reason:4})]);
add('missing-input',[cancel({inputId:'missing'})]);
add('stale-displayed-revision',[patch('tail',{state:'preparing'}),cancel({expectedRevision:1})]);
for(const value of [{finishedAt:at,state:'failed'},{result:{objectId:'result',revision:1}},{cancellation:{operationId:'other',callerPrincipalId:'bob',requestedAt:at,reason:'Other'}}])add('already-unavailable-'+cases.length,[patch('tail',value),cancel()]);
add('no-longer-queued',[{action:'remove-tail'},cancel()]);
add('operation-replay-does-not-replace-reason',[cancel(),cancel({replay:true,reason:'Changed reason'})]);
add('operation-replay-does-not-replace-target',[cancel(),cancel({replay:true,target:'head',inputId:'head'})]);
add('unresolved-cancellation-cannot-claim',[{action:'begin'}, {action:'patch-operation',value:{phase:'outcome_unknown'}},cancel({replay:true})]);
add('input-changed-before-intent-is-definitely-refused',[{action:'begin'},{action:'claim'},patch('tail',{state:'preparing'}),recover,cancel({replay:true})]);
add('foreign-pending-action-is-not-cleared',[{action:'claim-head-send'},recover]);
add('other-cancellation-claim-is-recovered',[{action:'begin',name:'other',target:'head'},{action:'claim',name:'other'},cancel()]);
for(const kind of ['before','after','contention'])add('input-write-'+kind,[{action:'fault',kind},cancel(),recover,intent,reopen,cancel({replay:true})]);
for(const refuseAt of [1,2,3])add('owner-refusal-'+refuseAt,[{action:'control',value:{refuseAt}},cancel(),{action:'control',value:{refuseAt:0}},cancel({replay:true}),recover]);
for(const change of [{reason:'Forged'},{callerPrincipalId:'other'},{requestedAt:'2026-01-01T00:00:00.000Z'}])add('retained-intent-identity-'+cases.length,[cancel(),{action:'patch-cancellation',value:change},intent]);
add('no-retained-intent',[intent]);
for(const stage of ['accept','claim','intent','finish','terminal','release'])add('original-crash-'+stage,[reopen,recover,cancel({replay:true}),intent],{crash:stage});
for(const c of cases){
 let seed=seeds[c.seed],root=mkdtempSync(join(tmpdir(),'ivy-cancellation-')),file=join(root,'chat-bridge.sqlite');writeFileSync(file,Buffer.from(seed.database,'base64'));
 let client=new Client(seed.records,seed.history),store,ops,queue,main,cancellation,ownerChecks=0,last=0;
 const events=[],controls={},requests=new Map(),aliases=new Map(),mutations=new Map();c.expected=[];
 function open(){store=new ChatStore(client,null,root);ops=new ChatOperations(store,admission);queue=new ChatQueue(ops);main={store,admission,operations:ops,queue,async ensure(){throw Error('Unexpected binding creation');},native:{async verifyOwner(){events.push({method:'verify'});ownerChecks++;if(controls.refuseAt===ownerChecks)throw new IvyError('fixture_owner_unavailable','Original owner is unavailable.');}},async recoverPending(){events.push({method:'recover'});const current=await queue.main();if(!current.value.pendingAction)return;const operation=await store.read('chat-bridge/operation',current.value.pendingAction);switch(operation.value.request.action){case'cancel':await cancellation.recoverPending();break;case'notify':await new ChatNotices(main).recoverPending();break;case'send':await queue.recoverPending();break;default:throw new IvyError('chat_pending_action','Unknown original Main claim.');}}};cancellation=new ChatCancellation(main);}
 function request(s={}){
  const name=s.name??'c',target=s.target??'tail',caller=s.caller??'alice';let value=requests.get(name);
  if(!s.replay||!value){const id=s.inputId&&s.inputId!=='head'?s.inputId:seed.inputs[target],record=client.records.find(r=>r.object.id===id);value={action:'cancel',operationId:`chat:${now}:cancel-${name}`,inputId:id,expectedRevision:s.expectedRevision??record?.revision.revision??1,expectedBridge:admission.expected(s.badBridge?'other':caller),reason:'Original cancellation'};requests.set(name,clone(value));}
  value=clone(value);if('reason'in s)value.reason=s.reason;if(s.inputId==='head')value.inputId=seed.inputs.head;if(s.omitBridge)delete value.expectedBridge;return value;
 }
 const update=async(key,doc,patch)=>store.write(key,{...doc.value,...patch},'fixture:'+doc.pin.objectId+':'+doc.pin.revision,{objectId:doc.pin.objectId,expectedRevision:doc.pin.revision});
 function normalize(v){if(typeof v==='string'){if(aliases.has(v))return aliases.get(v);for(const [id,name]of aliases)if(v.includes(id))v=v.replaceAll(id,name);return v;}if(Array.isArray(v))return v.map(normalize);if(v&&typeof v==='object')return Object.fromEntries(Object.entries(v).map(([k,v])=>[k,normalize(v)]));return v;}
 function snapshot(output){const db=new DatabaseSync(file),rows=db.prepare('SELECT d.id,d.key,d.name,d.parent_id,d.current_revision,r.value_json FROM documents d JOIN revisions r ON d.id=r.id AND d.current_revision=r.revision ORDER BY d.name,d.id').all();for(const r of rows)aliases.set(r.id,'<local:'+r.name+'>');for(const call of client.calls)if(call.params.mutationId){const id=call.params.mutationId;if(!mutations.has(id))mutations.set(id,'<mutation:'+mutations.size+'>');aliases.set(id,mutations.get(id));}const writes=db.prepare('SELECT rowid,key,id,revision,value_json FROM collector_test_writes WHERE rowid>? ORDER BY rowid').all(last);last=writes.at(-1)?.rowid??last;db.close();const answer=compact(normalize({output,documents:rows.map(r=>({id:r.id,key:r.key,name:r.name,parent:r.parent_id,revision:r.current_revision,value:JSON.parse(r.value_json)})),blobs:[],writes:writes.map(w=>({key:w.key,id:w.id,revision:w.revision,value:JSON.parse(w.value_json)})),records:client.records,calls:client.calls,events,gates:[]}));client.calls.length=0;events.length=0;return answer;}
 open();try{
  if(c.crash){const original=store.write.bind(store);let reached=false;store.write=async(key,value,id,destination)=>{const pin=await original(key,value,id,destination),stage=key==='chat-bridge/operation'?(value.phase==='accepted'?'accept':value.phase==='succeeded'?'finish':null):key==='chat-bridge/input'?(value.state==='cancelled'?'terminal':value.state==='cancel_requested'?'intent':null):key==='chat-bridge/main'?(value.pendingAction?'claim':'release'):null;if(stage===c.crash&&!reached){reached=true;throw new IvyError('outcome_unknown','Original cancellation write reply was lost.','unknown');}return pin;};c.interrupted=await result(()=>cancellation.cancel('alice',request()));if(!reached)throw Error('Original crash not reached '+c.crash);store.close();c.database=readFileSync(file).toString('base64');c.databaseHash=digest(Buffer.from(c.database,'base64'));c.records=clone(client.records);c.history=clone(client.history());c.requests=Object.fromEntries(requests);client=new Client(c.records,c.history);open();client.calls.length=0;events.length=0;ownerChecks=0;}
  for(const s of c.steps){const output=await result(async()=>{
   const name=s.name??'c',target=s.target??'tail';
   switch(s.action){
    case'cancel':return cancellation.cancel(s.caller??'alice',request(s));
    case'begin':return(await ops.begin(s.caller??'alice',request(s))).value;
    case'claim':{const r=requests.get(name),o=await ops.find(r.expectedBridge.callerPrincipalId,r.operationId);await update('chat-bridge/main',await queue.main(),{pendingAction:o.pin});return;}
    case'claim-head-send':{const input=await store.read('chat-bridge/input',seed.inputs.head),o=await ops.find(input.value.identity.senderPrincipalId,input.value.operationId);await update('chat-bridge/main',await queue.main(),{pendingAction:{objectId:o.pin.objectId,revision:1}});return;}
    case'recover':await cancellation.recoverPending();return;
    case'recover-intent':return cancellation.recover((await store.read('chat-bridge/input',seed.inputs[target])).value,seed.inputs[target]);
    case'patch-input':client.patch(seed.inputs[target],s.value);return;
    case'patch-cancellation':{const input=client.records.find(r=>r.object.id===seed.inputs[target]);client.patch(seed.inputs[target],{cancellation:{...input.content.value.cancellation,...s.value}});return;}
    case'patch-operation':{const r=requests.get(name),o=await ops.find(r.expectedBridge.callerPrincipalId,r.operationId);await update('chat-bridge/operation',o,s.value);return;}
    case'remove-tail':{const current=await queue.main();await update('chat-bridge/main',current,{queue:current.value.queue.filter(t=>t.inputId!==seed.inputs.tail)});return;}
    case'fault':client.fault={kind:s.kind,objectId:seed.inputs.tail};return;
    case'control':Object.assign(controls,s.value);return;
    case'reopen':store.close();open();return;
    default:throw Error('Unknown cancellation action '+s.action);
   }
  });c.expected.push(snapshot(output));}
  for(const label of ['queued-tail-head-and-original-receipt','notice-cancellation-finishes-producer','other-cancellation-claim-is-recovered'])if(c.label===label&&c.expected.some(s=>s.output.error))throw Error('Original successful cancellation failed '+label+': '+JSON.stringify(c.expected.map(s=>s.output)));
 }finally{store.close();rmSync(root,{recursive:true,force:true});}
}
process.stdout.write(JSON.stringify({now,definition,instructionAsset,seeds,cases}));
