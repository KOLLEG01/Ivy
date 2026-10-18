import './chat-store-loader.mjs';
import {registerHooks} from 'node:module';
import {mkdtempSync,readFileSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,basename} from 'node:path';
import {DatabaseSync} from 'node:sqlite';
import {buildInstructions} from '../../../tools/build/build-instructions.mjs';
const sdk='data:text/javascript;base64,'+Buffer.from([
 `export {canonical,hashJson} from '${new URL('../../../packages/contracts/src/canonical.ts',import.meta.url)}';`,
 `export {IvyError,requireThat} from '${new URL('../../../packages/contracts/src/errors.ts',import.meta.url)}';`,
 `export {validateChat} from '${new URL('../../../packages/contracts/src/chat-validation.ts',import.meta.url)}';`,
 `export {mainNotification} from '${new URL('../../../packages/sdk/src/main-chat.ts',import.meta.url)}';`,
].join('\n')).toString('base64');
registerHooks({resolve(s,c,n){if(c.parentURL?.endsWith('/notices.ts')&&s==='../../../packages/sdk/src/node.js')return{url:sdk,shortCircuit:true};return n(s,c);}});
const {ChatStore}=await import('../../../services/chat-bridge/src/store.ts');
const {ChatAdmission}=await import('../../../services/chat-bridge/src/admission.ts');
const {ChatOperations}=await import('../../../services/chat-bridge/src/operations.ts');
const {ChatQueue,mainName,inputName}=await import('../../../services/chat-bridge/src/queue.ts');
const {ChatNotices}=await import('../../../services/chat-bridge/src/notices.ts');
const {noticePrefix,attributedNoticeReply,noticePrompt,admittedNoticeInput}=await import('../../../services/chat-bridge/src/notice-input.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {hashJson,canonical}=await import('../../../packages/contracts/src/canonical.ts');
const {parseOperationId}=await import('../../../packages/contracts/src/operation-id.ts');
const {unpersistedNativePlan}=await import('../../fixtures/native-plan.ts');
const now=1790000000123,at=new Date(now).toISOString(),RealDate=Date;
globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[now]));}static now(){return now;}};
const channel={adapter:'whatsapp',accountId:'account',channelId:'channel'};
const definition={workspaceId:'chat',principalId:'chat-service',rootObjectId:null,project:{serviceNodeId:'owner',namespace:'codex',kind:'project',nativeId:'/synthetic/project'},nativePlan:unpersistedNativePlan(),channels:[{channel,displayName:'Channel'}]};
const baseAdmission=new ChatAdmission(definition),source={objectId:'source',revision:1};
const result=async(fn)=>{try{return{value:await fn()};}catch(error){const e=IvyError.from(error);return{error:{code:e.code,message:e.message,outcome:e.outcome}};}};
const pure=[];
const callers=['Feather.task-board','task-board','TASK-BOARD','TASK-BOARD.extra','my-task-boarder','prefix.task-board.suffix','taſk-board','tasK-board','notask-boarder','ENVY.automationService','coordinator','ordinaryService2API','env.namespace','abcDefGhi','./-_:','','...', 'ßeta','ﬃrst','ǰoule','𐐨sender','\ud800name','\udfffname','\nfoo','\rfoo','\u2028foo','\u2029foo',' spaced','worker\\path'];
const texts=['Private notice','  notice','\n\r\t notice','[Sender] Original',' \n[Sender] Original','[[Service] Sender] Original','[] missing','[\r] no prefix','[\n] no prefix','[\u2028] visible','[\ud800] visible','[[nested] visible','[open','[ ] visible','', '   ','\u0085[Sender] text','\u180e[Sender] text'];
for(const caller of callers){pure.push({kind:'prefix',caller,expected:await result(()=>noticePrefix(caller))});
 for(const text of texts)pure.push({kind:'attributed',caller,text,expected:await result(()=>attributedNoticeReply(caller,text))});}
for(const cp of [9,10,11,12,13,32,0x85,0xa0,0x1680,0x180e,0x2000,0x2001,0x2002,0x2003,0x2004,0x2005,0x2006,0x2007,0x2008,0x2009,0x200a,0x200b,0x2028,0x2029,0x202f,0x205f,0x3000,0xfeff]){
 for(const text of [String.fromCodePoint(cp)+'body',String.fromCodePoint(cp)+'[Sender] body'])pure.push({kind:'attributed',caller:'service',text,expected:await result(()=>attributedNoticeReply('service',text))});
}
// Prove the fixed ASCII /iu marker matcher needs exactly these non-ASCII folds.
const alphabet=[...new Set('taskboardautomationcoordinator')],expressions=alphabet.map(c=>new RegExp('^'+c+'$','iu')),specialFolds=[];
for(let cp=128;cp<=0x10ffff;cp++){const char=String.fromCodePoint(cp);for(let i=0;i<expressions.length;i++)if(expressions[i].test(char))specialFolds.push([cp,alphabet[i]]);}
if(JSON.stringify(specialFolds)!==JSON.stringify([[383,'s'],[8490,'k']]))throw Error('Unexpected Unicode fold equivalences for notice markers.');
const assetRoot=mkdtempSync(join(tmpdir(),'ivy-chat-notice-asset-'));let instructionAsset,instructionParts;
try{
 const files=buildInstructions({root:process.cwd(),outputRoot:assetRoot,component:'chat-bridge'});
 if(files.length!==2)throw Error('Expected Main and notice instruction assets.');
 instructionAsset=readFileSync(files.find(p=>basename(p)==='chat-notice-prompt.json')).toString('base64');
 instructionParts=JSON.parse(Buffer.from(instructionAsset,'base64').toString('utf8'));
 if(!Array.isArray(instructionParts)||instructionParts.length!==3||instructionParts.some(p=>typeof p!=='string'))throw Error('Invalid notice template asset.');
 for(const caller of ['task-board','Service.ßender','\ud800'])for(const text of ['', 'Original\n\udfff${noticePrefix(callerPrincipalId)}']){
  const expected=noticePrompt(caller,text),composed=instructionParts[0]+text+instructionParts[1]+noticePrefix(caller)+instructionParts[2];
  if(composed!==expected)throw Error('Built notice asset differs from original template.');
  pure.push({kind:'prompt',caller,text,expected:{value:expected}});
 }
}finally{rmSync(assetRoot,{recursive:true,force:true});}
const admittedBase={action:'notify',operationId:`chat:${now}:admitted`,source,text:'Original\n\ud800',expectedBridge:baseAdmission.expected('producer'),channel};
const admittedCases=[];
const addAdmitted=(label,edit=()=>{},binding={objectId:'binding',revision:1},caller='producer')=>{const request=structuredClone(admittedBase);edit(request);admittedCases.push({label,caller,request,binding});};
addAdmitted('valid');addAdmitted('retained-hash',v=>v.expectedBridge.definitionHash='sha256:'+'a'.repeat(64));
for(const field of ['principalId','callerPrincipalId','workspaceId'])addAdmitted('wrong-bridge-'+field,v=>v.expectedBridge[field]='other');
addAdmitted('missing-bridge',v=>delete v.expectedBridge);addAdmitted('null-bridge',v=>v.expectedBridge=null);
addAdmitted('shape-after-authorization',v=>{v.expectedBridge.workspaceId='other';v.text=4;});
for(const text of ['',4,null,{},'x'.repeat(20000)])addAdmitted('text-'+JSON.stringify(text).slice(0,20),v=>v.text=text);
addAdmitted('foreign-channel-remains-admitted',v=>v.channel.channelId='retained');
addAdmitted('unknown-field',v=>v.unknown=true);addAdmitted('wrong-action',v=>v.action='send');
for(const binding of [null,{},4,{objectId:'\ud800',revision:2}])addAdmitted('binding-hashed-without-extra-admission',()=>{},binding);
for(const c of admittedCases)c.expected=await result(()=>admittedNoticeInput(baseAdmission,c.caller,c.request,c.binding));

const record=(id,key,value,parentId=null,name=id)=>({object:{id,parentId,name,contractKey:key,effectivelyArchived:false,createdAt:at},revision:{revision:1,contractVersion:'1.3.0',mediaType:'application/json'},content:{encoding:'json',value}});
class Client{
 constructor(records=[]){this.records=structuredClone(records);this.versions=new Map(this.records.map(r=>[r.object.id,new Map([[r.revision.revision,structuredClone(r)]]) ]));this.calls=[];this.fault=null;this.epochWarm=false;}
 async request(method,p){
  const logged=structuredClone(p);if(method==='objects.write'){if(parseOperationId(p.mutationId).runtimeEpoch!=='chat')throw Error('Wrong mutation epoch.');}
  this.calls.push({method,params:logged});
  if(method==='system.status'){this.epochWarm=true;return{runtimeEpoch:'chat'};}
  if(method==='objects.query'){
   const filters=p.where.args,name=filters.find(f=>f.field==='object.name').value,parent=filters[0].value??null;
   return{items:this.records.filter(r=>r.object.contractKey===p.contractKey&&r.object.parentId===parent&&r.object.name===name).map(r=>({objectId:r.object.id})),nextCursor:null};
  }
  if(method==='objects.read'){
   const current=this.records.find(r=>r.object.id===p.objectId);if(!current)throw new IvyError('object_missing','Fixture object is missing.');
   const selected=p.revision===undefined?current:this.versions.get(p.objectId)?.get(p.revision);if(!selected)throw new IvyError('revision_pruned','Fixture revision is unavailable.');return structuredClone(selected);
  }
  if(method!=='objects.write')throw Error('Unexpected RPC '+method);
  const isInput=p.create?.contractKey==='chat-bridge/input';
  if(isInput&&this.fault==='contention-input')throw new IvyError('revision_conflict','Fixture competing writer retained the input name.');
  if(isInput&&this.fault==='before-input'){this.fault=null;throw new IvyError('fixture_unavailable','Fixture input write was not executed.');}
  let current;
  if(p.create){
   if(this.records.some(r=>r.object.name===p.create.name&&r.object.parentId===p.create.parentId&&r.object.contractKey===p.create.contractKey))throw new IvyError('revision_conflict','Fixture name already exists.');
   current=record('fixture-record-'+(this.records.length+1),p.create.contractKey,structuredClone(p.content.value),p.create.parentId,p.create.name);current.revision.contractVersion=p.contractVersion;this.records.push(current);this.versions.set(current.object.id,new Map());
  }else{
   current=this.records.find(r=>r.object.id===p.objectId);if(!current)throw Error('Unknown fixture write.');
   if(current.revision.revision!==p.expectedRevision)throw new IvyError('revision_conflict','Fixture revision changed.');current.revision.revision++;current.content=structuredClone(p.content);
  }
  this.versions.get(current.object.id).set(current.revision.revision,structuredClone(current));
  if(isInput&&this.fault==='after-input'){this.fault=null;throw new IvyError('outcome_unknown','Fixture reply was lost after the input committed.','unknown');}
  return structuredClone(current);
 }
}
const baseRoot=mkdtempSync(join(tmpdir(),'ivy-chat-notice-seed-')),seedStore=new ChatStore({},null,baseRoot);
const bindingValue={schemaVersion:1,workspaceId:'chat',definitionHash:baseAdmission.definitionHash,project:definition.project,primary:{...definition.project,kind:'thread',nativeId:'fixture-main-thread'},nativePlan:definition.nativePlan,createdAt:at,operationId:'main-creation',predecessor:null};
const binding=await seedStore.write('chat-bridge/binding',bindingValue,'seed-binding',{create:{parentId:null,name:'fixture-binding'}});
const mainValue={schemaVersion:1,definition,definitionHash:baseAdmission.definitionHash,binding,conversation:{objectId:'conversation',revision:1},pendingAction:null,publication:null,queue:[],nextSequence:1,updatedAt:at};
const mainPin=await seedStore.write('chat-bridge/main',mainValue,'seed-main',{create:{parentId:null,name:mainName(definition)}});
seedStore.close();
{
 const db=new DatabaseSync(join(baseRoot,'chat-bridge.sqlite'));
 db.exec(`CREATE TABLE notice_test_writes(key TEXT,value_json TEXT);
 CREATE TRIGGER notice_test_revision AFTER INSERT ON revisions BEGIN INSERT INTO notice_test_writes SELECT d.key,NEW.value_json FROM documents d WHERE d.id=NEW.id; END;`);db.close();
}
const database=readFileSync(join(baseRoot,'chat-bridge.sqlite')).toString('base64');rmSync(baseRoot,{recursive:true,force:true});
const requests={a:{action:'notify',operationId:`chat:${now}:a`,source,text:'Original task notification'},b:{action:'notify',operationId:`chat:${now}:b`,source,text:'Second notice'},changed:{action:'notify',operationId:`chat:${now}:a`,source,text:'Changed content'}};
const full=Array.from({length:64},(_,i)=>({inputId:'retained-'+i,identity:{channel,senderPrincipalId:'other',messageId:'retained-'+i},requestHash:'sha256:'+'1'.repeat(64),binding,sequence:i+1,admittedAt:at}));
const notify=(name='a',caller='producer')=>({action:'notify',name,caller});
const begin=(name='a')=>({action:'begin',name});
const claim={action:'claim'},recover={action:'recover'},reopen={action:'reopen'},finish={action:'finish-undelivered'},query={action:'notice',name:'a',caller:'producer'};
const patchMain=patch=>({action:'patch-main',patch}),patchInput=patch=>({action:'patch-input',patch}),patchOperation=patch=>({action:'patch-operation',patch});
const cases=[];const add=(label,steps,extra={})=>cases.push({label,steps,...extra});
add('new-notice-replay-and-producer-isolation',[notify(),notify(),reopen,notify(),notify('b'),notify('a','other'),notify('changed')]);
add('retained-input-does-not-recheck-owner',[notify(),{action:'owner',refused:true},notify(),query]);
add('owner-refusal-retains-no-operation',[{action:'owner',refused:true},notify(),{action:'owner',refused:false},notify()]);
add('automatic-main-must-publish-binding',[patchMain({binding:null}),notify(),{action:'ensure',publishBinding:true},notify()]);
add('new-full-queue-refused-before-admission',[patchMain({queue:full,nextSequence:65}),notify()]);
add('sequence-exhaustion-before-admission',[patchMain({nextSequence:9007199254740991}),notify()]);
add('source-not-found-before-admission',[{action:'source',missing:true},notify()]);
add('archived-source-before-admission',[{action:'source',archived:true},notify()]);
add('pruned-source-before-admission',[{action:'source',pruned:true},notify()]);
add('existing-accepted-rechecks-owner',[begin(),{action:'owner',refused:true},notify()]);
for(const phase of ['succeeded','failed','outcome_unknown'])add('terminal-'+phase+'-does-not-recheck-owner',[begin(),patchOperation({phase}),{action:'owner',refused:true},notify()]);
add('dispatching-requires-original-reconciliation',[begin(),patchOperation({phase:'dispatching'}),notify()]);
add('accepted-full-refusal-under-claim',[begin(),claim,patchMain({queue:full,nextSequence:65}),recover,notify()]);
add('accepted-without-binding-refusal',[begin(),claim,patchMain({binding:null}),recover,notify()]);
add('other-action-claim-is-not-a-notice',[{action:'begin-other'},{action:'claim-other'},recover]);
add('held-pending-main-has-bounded-retry',[{action:'begin-other'},{action:'claim-other'},{action:'hold-pending',value:true},notify()]);
add('input-write-not-executed',[{action:'fault',when:'before-input'},notify(),reopen,recover,notify()]);
add('input-reply-lost-after-commit',[{action:'fault',when:'after-input'},notify(),reopen,recover,notify()]);
add('recover-after-thirty-two-input-conflicts',[{action:'fault',when:'contention-input'},notify(),{action:'fault',when:null},reopen,recover,notify()]);
add('recover-after-thirty-two-claim-conflicts',[{action:'claim-contention',value:true},notify(),{action:'claim-contention',value:false},reopen,notify()]);
add('default-undelivered-cancellation-is-idempotent',[notify(),patchInput({state:'cancelled',finishedAt:at,updatedAt:at}),finish,finish,reopen,notify(),query]);
add('failed-undelivered-error-is-immutable',[notify(),patchInput({state:'failed',finishedAt:at,error:{code:'fixture_failed',detail:'Original failure.',outcome:'unknown'}}),finish,patchInput({error:{code:'different',detail:'Changed failure.',outcome:'not_executed'}}),finish,query]);
for(const [label,patch]of [['still-queued',{}],['native-turn',{state:'cancelled',finishedAt:at,turnId:'native-turn'}],['native-result',{state:'failed',finishedAt:at,result:{objectId:'result',revision:1}}],['missing-time',{state:'failed'}],['completed',{state:'completed',finishedAt:at}]])add('reject-undelivered-'+label,[notify(),patchInput(patch),finish]);
add('stale-undelivered-pin-is-not-settled',[notify(),{action:'retain-input'},patchInput({state:'cancelled',finishedAt:at}),{...finish,stale:true}]);
add('non-notice-input-is-not-settled',[{action:'begin-other'},{action:'finish-other'}]);
add('producer-cannot-query-another-notice',[notify(),{...query,caller:'other'},query]);
add('notice-reads-original-native-reply',[notify(),{action:'publish',name:'a'},query,reopen,query]);
add('rewritten-native-answer-is-not-delivery-proof',[notify(),{action:'publish',name:'a'},{action:'patch-reply',patch:{text:'Rewritten content'}},query]);
add('native-reply-survives-definition-change',[notify(),{action:'publish',name:'a'},{action:'definition-change'},query,notify()]);
add('notice-user-queue-order',[notify(),{action:'send-user'},notify('b')]);
add('notify-extra-channel-is-refused-before-io',[{action:'notify-raw',request:{...requests.a,channel:{...channel,channelId:'unconfigured'}}}]);
add('notice-checks-exact-input-answer',[notify(),notify('b'),{action:'publish',name:'b'},{action:'finish-with-reply',name:'a',replyName:'b'},query]);
add('notice-refuses-final-notice-origin',[notify(),{action:'publish-final'},query]);
add('retained-input-survives-definition-change',[notify(),{action:'definition-change'},notify()]);
for(const field of ['workspaceId','definitionHash','requestHash','operationId','payload','identity'])add('retained-input-integrity-'+field,[notify(),patchInput({[field]:field==='payload'?{text:'Changed',images:[]}:field==='identity'?{channel,senderPrincipalId:'producer',messageId:'changed'}:'changed'}),notify()]);
for(const stage of ['accept','claim','publish-input','publish-ticket','release'])add('original-crash-'+stage,[reopen,recover,notify()],{crash:stage});
for(const [label,patch]of [['id',{inputId:'changed'}],['hash',{requestHash:'sha256:'+'2'.repeat(64)}],['binding',{binding:{objectId:'changed',revision:1}}],['sequence',{sequence:0}],['timestamp',{admittedAt:'2026-01-01T00:00:00.000Z'}]])add('claimed-ticket-'+label,[{action:'patch-ticket',patch},reopen,recover],{crash:'publish-ticket'});
add('claimed-input-cannot-consume-next-sequence',[patchMain({nextSequence:2}),reopen,recover],{crash:'publish-input'});
add('changed-binding-under-claim',[patchMain({binding:{objectId:'replacement',revision:1}}),reopen,recover],{crash:'publish-input'});

// The fixture runner below uses real production stores/receipts and only a
// synthetic native/Main binding owner plus a narrow public Hive record service.
for(const c of cases){
 const root=mkdtempSync(join(tmpdir(),'ivy-chat-notices-'));writeFileSync(join(root,'chat-bridge.sqlite'),Buffer.from(database,'base64'));
 const client=new Client([record('source','domain/source',{original:true})]);
 let store=new ChatStore(client,null,root),admission=new ChatAdmission(definition),ops=new ChatOperations(store,admission),other=null,stale=null;
 const control={ownerRefused:false,publishBinding:false,holdPending:false,claimContention:false,verifyCount:0};
 const trace=(method,params={})=>client.calls.push({method,params:structuredClone(params)});
 const rawMain=()=>store.read('chat-bridge/main',mainPin.objectId);
 const update=async(key,doc,patch)=>store.write(key,{...doc.value,...patch},'fixture-update',{objectId:doc.pin.objectId,expectedRevision:doc.pin.revision});
 const requestFor=(name,caller='producer')=>({...structuredClone(requests[name]),expectedBridge:admission.expected(caller),channel:structuredClone(channel)});
 const operation=(name='a',caller='producer')=>ops.find(caller,requests[name].operationId);
 const input=(name='a')=>store.named('chat-bridge/input',inputName(definition,{channel,senderPrincipalId:'producer',messageId:requests[name].operationId},baseAdmission.definitionHash));
 const main={get admission(){return admission;},get operations(){return ops;},get store(){return store;},get queue(){return new ChatQueue(ops);},
  native:{async verifyOwner(){trace('native.verifyOwner');control.verifyCount++;if(control.ownerRefused)throw new IvyError('fixture_owner_refused','The original native owner is unavailable.');if(control.claimContention&&control.verifyCount>1)await update('chat-bridge/main',await rawMain(),{});}},
  async ensure(caller,operationId,mode){trace('main.ensure',{caller,operationId,mode});if(control.publishBinding)await update('chat-bridge/main',await rawMain(),{binding});},
  async recoverPending(){trace('main.recoverPending');if(!control.holdPending)await new ChatNotices(main).recoverPending();}
 };
 const notices=()=>new ChatNotices(main);
 let lastWrite=0;const mutationIds=new Map();
 const snapshot=async(output)=>{
  const documents={},ids=new Map();
  for(const caller of ['producer','other'])for(const name of Object.keys(requests)){
   const doc=await ops.find(caller,requests[name].operationId);if(doc){documents[caller+':'+name]={value:doc.value,revision:doc.pin.revision};ids.set(doc.pin.objectId,'operation:'+caller+':'+requests[name].operationId);}
  }
  if(other)ids.set(other.pin.objectId,'operation:other');
  const db=new DatabaseSync(join(root,'chat-bridge.sqlite'));
  const writes=db.prepare('SELECT rowid,key,value_json FROM notice_test_writes WHERE rowid>? ORDER BY rowid').all(lastWrite);lastWrite=writes.at(-1)?.rowid??lastWrite;db.close();
  const normalize=value=>{if(typeof value==='string')return ids.get(value)??value;if(Array.isArray(value))return value.map(normalize);if(value&&typeof value==='object')return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,normalize(v)]));return value;};
  const saved=await rawMain();
  const answer=normalize({output,main:{value:saved.value,revision:saved.pin.revision},operations:documents,records:client.records,writes:writes.map(r=>({key:r.key,value:JSON.parse(r.value_json)})),calls:client.calls});
  for(const call of answer.calls)if(call.method==='objects.write'){const id=call.params.mutationId;if(!mutationIds.has(id))mutationIds.set(id,'<mutation:'+mutationIds.size+'>');call.params.mutationId=mutationIds.get(id);}
  client.calls=[];return answer;
 };
 const patchInputRecord=async(patch,name='a')=>{const doc=await input(name),r=client.records.find(r=>r.object.id===doc.pin.objectId);r.revision.revision++;r.content.value={...r.content.value,...structuredClone(patch)};client.versions.get(r.object.id).set(r.revision.revision,structuredClone(r));};
 const publish=async(name='a',final=false)=>{
  const op=await operation(name);let value,reply;
  if(final){
   value={schemaVersion:1,workspaceId:'chat',definitionHash:admission.definitionHash,channel,origin:{kind:'notice',operation:{objectId:'final-producer',revision:1},claim:{objectId:'final-claim',revision:1},producerPrincipalId:'producer',source},sequence:2,partIndex:0,text:'Final notice',artifacts:[],createdAt:at};
   reply=await store.write('chat-bridge/reply',{...value,immutableHash:hashJson(value),state:'pending',firstOfferedAt:null,confirmedAt:null},'fixture-final',{create:{parentId:null,name:'Chat notice final-producer'}});
  }else{
   const original=await input(name),binding=original.value.binding;
   const result=await store.write('chat-bridge/result',{schemaVersion:1,inputId:original.pin.objectId,binding,turnId:'fixture-turn',nativeState:'completed',evidence:[original.pin],textParts:[],createdAt:at},'fixture-result',{create:{parentId:original.pin.objectId,name:'Fixture result '+name}});
   await patchInputRecord({state:'completed',result,turnId:'fixture-turn',finishedAt:at,updatedAt:at},name);
   value={schemaVersion:1,workspaceId:'chat',definitionHash:admission.definitionHash,channel,origin:{kind:'native',inputId:original.pin.objectId,binding,result},sequence:original.value.sequence+1,partIndex:0,text:'Native answer '+name,artifacts:[],createdAt:at};
   reply=await store.write('chat-bridge/reply',{...value,immutableHash:hashJson(value),state:'pending',firstOfferedAt:null,confirmedAt:null},'fixture-reply',{create:{parentId:null,name:'Fixture reply '+name}});
  }
  const doc=await store.read('chat-bridge/reply',reply);await ops.finish(op.pin.objectId,{action:'notify',operationId:op.value.operationId,reply:{object:doc.pin,data:doc.value}});
 };
 try{
  if(c.crash){
   const original=store.write.bind(store);let stopped=false;
   store.write=async(key,value,mutation,destination)=>{
    const pin=await original(key,value,mutation,destination);
    const stage=key==='chat-bridge/operation'&&value.phase==='accepted'?'accept':key==='chat-bridge/input'?'publish-input':key==='chat-bridge/main'?(value.pendingAction?(value.queue.length?'publish-ticket':'claim'):'release'):null;
    if(stage===c.crash&&!stopped){stopped=true;throw new IvyError('outcome_unknown','Fixture reply was lost after '+stage+'.','unknown');}return pin;
   };
   c.interrupted=await result(()=>notices().notify('producer',requests.a));if(!stopped)throw Error('Original crash barrier was not reached: '+c.label);
   c.scopeCacheWarm=client.calls.some(c=>c.method==='objects.write');store.close();c.database=readFileSync(join(root,'chat-bridge.sqlite')).toString('base64');c.records=structuredClone(client.records);
   store=new ChatStore(client,null,root);ops=new ChatOperations(store,admission);lastWrite=0;
   {const db=new DatabaseSync(join(root,'chat-bridge.sqlite'));lastWrite=db.prepare('SELECT COALESCE(MAX(rowid),0) AS n FROM notice_test_writes').get().n;db.close();}
  }
  c.initialWrite=lastWrite;client.calls=[];c.expected=[];
  for(const step of c.steps){
   const output=await result(async()=>{switch(step.action){
    case 'notify':return notices().notify(step.caller,requests[step.name]);
    case 'notify-raw':return notices().notify('producer',step.request);
    case 'begin':return(await ops.begin('producer',requestFor(step.name))).value;
    case 'claim':await update('chat-bridge/main',await rawMain(),{pendingAction:(await operation()).pin});return null;
    case 'recover':await notices().recoverPending();return null;
    case 'owner':control.ownerRefused=step.refused;return null;
    case 'ensure':control.publishBinding=step.publishBinding;return null;
    case 'hold-pending':control.holdPending=step.value;return null;
    case 'claim-contention':control.claimContention=step.value;return null;
    case 'fault':client.fault=step.when;return null;
    case 'patch-main':await update('chat-bridge/main',await rawMain(),step.patch);return null;
    case 'patch-operation':await update('chat-bridge/operation',await operation(),step.patch);return null;
    case 'patch-input':await patchInputRecord(step.patch);return null;
    case 'patch-reply':{const op=await operation(),reply=await store.read('chat-bridge/reply',op.value.outcome.reply.object);await update('chat-bridge/reply',reply,step.patch);return null;}
    case 'patch-ticket':{const doc=await rawMain();await update('chat-bridge/main',doc,{queue:[{...doc.value.queue[0],...step.patch}]});return null;}
    case 'source':{const r=client.records.find(r=>r.object.id==='source');if(step.missing){client.records=client.records.filter(r=>r!==r);return null;}if(step.archived)r.object.effectivelyArchived=true;if(step.pruned)r.revision.revision=2;client.versions.get('source').set(1,structuredClone(r));return null;}
    case 'definition-change':{const changed=structuredClone(definition);changed.channels[0].displayName='Changed';admission=new ChatAdmission(changed);ops=new ChatOperations(store,admission);return null;}
    case 'begin-other':{other=await ops.begin('producer',{action:'acknowledge',operationId:`chat:${now}:other-action`,expectedBridge:admission.expected('producer'),offerId:'offer',replyIds:['reply']});return null;}
    case 'claim-other':await update('chat-bridge/main',await rawMain(),{pendingAction:other.pin});return null;
    case 'retain-input':stale=await input();return null;
    case 'finish-undelivered':await notices().finishUndelivered(step.stale?stale:await input());return null;
    case 'finish-other':await notices().finishUndelivered({pin:{objectId:'synthetic-input',revision:1},value:{identity:{senderPrincipalId:'producer'},operationId:other.value.operationId}});return null;
    case 'notice':return notices().notice(step.caller,{operationId:requests[step.name].operationId});
    case 'publish':await publish(step.name);return null;
    case 'publish-final':await publish('a',true);return null;
    case 'send-user':{const request={action:'send',operationId:`chat:${now}:user`,expectedBridge:admission.expected('user'),channel,messageId:'user-message',expectedBinding:binding,payload:{text:'User input',images:[]}};const value=await main.queue.send('user',request);other=await ops.find('user',request.operationId);return value;}
    case 'finish-with-reply':{const op=await operation(step.name),other=await operation(step.replyName);await ops.finish(op.pin.objectId,{...other.value.outcome,operationId:op.value.operationId});return null;}
    case 'reopen':store.close();store=new ChatStore(client,null,root);ops=new ChatOperations(store,admission);return null;
    default:throw Error('Unknown fixture action '+step.action);
   }});
   c.expected.push(await snapshot(output));
  }
 }finally{store.close();rmSync(root,{recursive:true,force:true});}
 if(c.label==='new-notice-replay-and-producer-isolation'&&c.expected.at(-1).records.filter(r=>r.object.contractKey==='chat-bridge/input').length!==3)throw Error('Notice replay/producer isolation was not exercised.');
 if(c.label==='notice-reads-original-native-reply'&&!c.expected.at(-1).output.value?.data?.origin?.inputId)throw Error('Original native reply was not exercised.');
 if(c.label==='notice-user-queue-order'&&JSON.stringify(c.expected.at(-1).main.value.queue.map(v=>v.sequence))!=='[1,2,3]')throw Error('User and notice queue order was not exercised.');
 if(c.label==='recover-after-thirty-two-input-conflicts'&&c.expected[1].calls.filter(v=>v.method==='objects.write').length!==32)throw Error('Input contention bound was not exercised.');
 if(c.label==='recover-after-thirty-two-claim-conflicts'&&c.expected[1].output.error?.code!=='chat_contention')throw Error('Claim contention bound was not exercised.');
}
const races=[];
for(const [label,names,filled]of [['same-operation',['a','a'],false],['different-operations',['a','b'],false],['last-queue-slot',['a','b'],true]]){
 const root=mkdtempSync(join(tmpdir(),'ivy-chat-notice-race-'));writeFileSync(join(root,'chat-bridge.sqlite'),Buffer.from(database,'base64'));
 const client=new Client([record('source','domain/source',{original:true})]),stores=[new ChatStore(client,null,root),new ChatStore(client,null,root)],operations=stores.map(s=>new ChatOperations(s,baseAdmission));
 const mains=operations.map((ops,i)=>{const main={admission:baseAdmission,operations:ops,store:stores[i],queue:new ChatQueue(ops),native:{async verifyOwner(){}},async ensure(){},async recoverPending(){await new ChatNotices(main).recoverPending();}};return main;});
 try{
  if(filled){const m=await stores[0].read('chat-bridge/main',mainPin.objectId);await stores[0].write('chat-bridge/main',{...m.value,queue:full.slice(0,63),nextSequence:64},'fill',{objectId:m.pin.objectId,expectedRevision:m.pin.revision});}
  for(const [i,name]of names.entries())await operations[i].begin('producer',{...requests[name],expectedBridge:baseAdmission.expected('producer'),channel});
  const replies=await Promise.all(mains.map((m,i)=>new ChatNotices(m).notify('producer',requests[names[i]])));
  await new ChatNotices(mains[0]).recoverPending();const saved=(await mains[0].queue.main()).value;
  races.push({label,names,filled,expected:{phases:replies.map(v=>v.phase).sort(),errors:replies.filter(v=>v.phase==='failed').map(v=>v.error.code).sort(),queueLength:saved.queue.length,nextSequence:saved.nextSequence,pending:saved.pendingAction!==null,inputCount:client.records.filter(r=>r.object.contractKey==='chat-bridge/input').length}});
 }finally{for(const s of stores)s.close();rmSync(root,{recursive:true,force:true});}
}
process.stdout.write(JSON.stringify({now,definition,channel,source,binding,bindingValue,mainPin,mainValue,database,requests,full,pure,admittedCases,instructionAsset,specialFolds,cases,races}));
