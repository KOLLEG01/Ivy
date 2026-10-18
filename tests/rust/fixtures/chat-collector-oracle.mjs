import {now,RealDate,instructions,seeds,bodies,summary,error,result} from './chat-native-evidence-oracle.mjs';
import {registerHooks} from 'node:module';
import {mkdtempSync,readFileSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {DatabaseSync} from 'node:sqlite';
// These are the remaining Main/delivery and native transport boundaries. All
// collector, evidence, reducer, store, Result and publisher code is original.
registerHooks({resolve(s,c,n){
 if(c.parentURL?.endsWith('/collector.ts')&&s==='./chat-main.js')return{url:'data:text/javascript,export const ChatMain=undefined;',shortCircuit:true};
 if(c.parentURL?.endsWith('/collector.ts')&&s==='./delivery.js')return{url:'data:text/javascript,export class ChatDelivery{constructor(main){this.head=(id)=>main.fixtureHead(id);}}',shortCircuit:true};
 return n(s,c);
}});
const {ChatStore}=await import('../../../services/chat-bridge/src/store.ts');
const {ChatAdmission}=await import('../../../services/chat-bridge/src/admission.ts');
const {ChatOperations}=await import('../../../services/chat-bridge/src/operations.ts');
const {ChatCollector}=await import('../../../services/chat-bridge/src/collector.ts');
const {ChatPublisher}=await import('../../../services/chat-bridge/src/publisher.ts');
const {ChatNativeEvidence}=await import('../../../services/chat-bridge/src/native-evidence.ts');
const {ChatQueue,mainName}=await import('../../../services/chat-bridge/src/queue.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {canonical,hashJson,digest}=await import('../../../packages/contracts/src/canonical.ts');
const {checkedNativeContract}=await import('../../../packages/contracts/src/checked-native-contract.ts');
const {deriveOperationId,parseOperationId}=await import('../../../packages/contracts/src/operation-id.ts');
const at=new RealDate(now).toISOString(),probe=Symbol.for('ivy.test.chat-native-evidence-gate'),definitions={};
let clock=now;
globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[clock]));}static now(){return clock;}};
const compact=v=>typeof v==='string'&&v.length>512?{utf16Length:v.length,utf16Hash:digest(Buffer.from(v,'utf16le')),utf8Length:Buffer.byteLength(v),utf8Hash:digest(Buffer.from(v))}:Array.isArray(v)?v.map(compact):v&&typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,v])=>[k,compact(v)])):v;
const clone=v=>structuredClone(v);
class Client{
 constructor(records){this.records=clone(records);this.versions=new Map(this.records.map(r=>[r.object.id,new Map([[r.revision.revision,clone(r)]])]));this.calls=[];this.receipts=new Map();this.fault=null;}
 async request(method,p){
  this.calls.push({method,params:clone(p)});
  if(method==='system.status')return{runtimeEpoch:'chat'};
  if(method==='objects.read'||method==='objects.stat'){
   const current=this.records.find(r=>r.object.id===p.objectId);if(!current)throw new IvyError('object_missing','Original object is unavailable.');
   const selected=p.revision===undefined?current:this.versions.get(p.objectId)?.get(p.revision);if(!selected)throw new IvyError('revision_pruned','Original revision is unavailable.');return clone(method==='objects.stat'?selected.object:selected);
  }
  if(method==='objects.query'){
   const args=p.where.args,parent=args[0].value??null,name=args[1].value,selected=this.records.filter(r=>r.object.contractKey===p.contractKey&&r.object.parentId===parent&&r.object.name===name);
   return{items:selected.map(r=>({objectId:r.object.id,revision:r.revision.revision})),nextCursor:null};
  }
  if(method!=='objects.write')throw Error('Unexpected RPC '+method);
  if(parseOperationId(p.mutationId).runtimeEpoch!=='chat')throw Error('Wrong mutation epoch');
  if(this.receipts.has(p.mutationId))return clone(this.receipts.get(p.mutationId));
  const fault=this.fault&&(this.fault.key===p.create?.contractKey||this.fault.objectId===p.objectId)?this.fault:null;
  if(fault){this.fault=null;if(fault.kind==='before')throw new IvyError('fixture_unavailable','Original write was not executed.');if(fault.kind==='contention')throw new IvyError('revision_conflict','Original revision changed.');}
  let saved;
  if(p.create){
   if(this.records.some(r=>r.object.contractKey===p.create.contractKey&&r.object.parentId===p.create.parentId&&r.object.name===p.create.name))throw new IvyError('revision_conflict','Original name already exists.');
   const text=p.content.encoding==='text';
   saved={object:{id:'collector-public-'+(this.records.length+1),parentId:p.create.parentId,ownerObjectId:p.create.ownerObjectId,name:p.create.name,contractKey:p.create.contractKey,effectivelyArchived:false},revision:{revision:1,contractVersion:p.contractVersion,mediaType:text?'text/plain':'application/json',...(text?{byteLength:Buffer.byteLength(p.content.value),contentHash:digest(Buffer.from(p.content.value))}:{})},content:clone(p.content)};
   this.records.push(saved);this.versions.set(saved.object.id,new Map());
  }else{
   saved=this.records.find(r=>r.object.id===p.objectId);if(!saved)throw new IvyError('object_missing','Original object is unavailable.');
   if(saved.revision.revision!==p.expectedRevision)throw new IvyError('revision_conflict','Original revision changed.');saved.revision.revision++;saved.content=clone(p.content);
  }
  this.versions.get(saved.object.id).set(saved.revision.revision,clone(saved));this.receipts.set(p.mutationId,clone(saved));
  if(fault?.kind==='after')throw new IvyError('outcome_unknown','Original reply was lost after commit.','unknown');return clone(saved);
 }
}
globalThis[probe]=(kind,input,fn)=>fn();
for(const version of ['0.154.0','0.142.3','0.158.0','0.159.2']){
 const original=seeds[version],root=mkdtempSync(join(tmpdir(),'ivy-collector-seed-'));writeFileSync(join(root,'chat-bridge.sqlite'),Buffer.from(original.database,'base64'));
 const client=new Client([original.input]),store=new ChatStore(client,null,root),definition=clone(original.definition),admission=new ChatAdmission(definition);let body;
 try{
  body=await bodies(store,definition,client.records[0]);
  const binding=await store.write('chat-bridge/binding',{schemaVersion:1,workspaceId:'chat',definitionHash:admission.definitionHash,project:definition.project,primary:{...definition.project,kind:'thread',nativeId:'original-thread'},nativePlan:definition.nativePlan,createdAt:at,operationId:'original-binding',predecessor:null},'seed',{create:{parentId:null,name:'Original binding'}});
  Object.assign(client.records[0].content.value,{binding});
  const evidence=new ChatNativeEvidence(store,definition),request=await evidence.saveRequest('original-input',body.request);
  Object.assign(body.call,{operationId:deriveOperationId(client.records[0].content.value.operationId,'chat:native:turn/start'),request,preparedEpoch:null});
  body.operation.operationId=body.call.operationId;
  body.call.evidence=await evidence.saveOperation('original-input',body.call,body.operation);
  Object.assign(body.call,{state:'succeeded',epoch:'original-epoch'});
  const call=await store.write('chat-bridge/native-call',body.call,'seed',{create:{parentId:'original-input',name:'Original native turn'}});
  client.records[0].content.value.nativeCalls.turn=call;
  const mainPin=await store.write('chat-bridge/main',{schemaVersion:1,definition,definitionHash:admission.definitionHash,binding,conversation:{objectId:'conversation',revision:1},pendingAction:null,publication:null,queue:[{inputId:'original-input',identity:client.records[0].content.value.identity,sequence:1}],nextSequence:2,updatedAt:at},'seed',{create:{parentId:null,name:mainName(definition)}});
  store.close();
  const db=new DatabaseSync(join(root,'chat-bridge.sqlite'));db.exec(`CREATE TABLE collector_test_writes(key TEXT,id TEXT,revision INTEGER,value_json TEXT);CREATE TRIGGER collector_test_revision AFTER INSERT ON revisions BEGIN INSERT INTO collector_test_writes SELECT key,NEW.id,NEW.revision,NEW.value_json FROM documents WHERE id=NEW.id; END;`);db.close();
  definitions[version]={definition,database:readFileSync(join(root,'chat-bridge.sqlite')).toString('base64'),records:client.records,binding,call,mainPin};
 }finally{try{store.close();}catch{}rmSync(root,{recursive:true,force:true});}
}
const step={action:'step'},reopen={action:'reopen'},terminal={action:'terminal'},rounds=n=>Array.from({length:n},()=>step),cases=[];
const control=value=>({action:'control',value}),patch=(target,value)=>({action:'patch',target,value}),add=(label,steps,extra={})=>cases.push({label,version:'0.154.0',steps,...extra});
const page=(status='completed')=>({reply:{result:{data:[{id:'original-turn',status,items:[],itemsView:'notLoaded'}],nextCursor:null}}});
const item=(text='Original reply\n😀',id='answer')=>({reply:{result:{data:[{turnId:'original-turn',item:{id,type:'agentMessage',text}}],nextCursor:null}}});
const success=[page(),item(),page()];
add('collect-reopen-publish-and-release',[terminal,step,reopen,terminal,step,reopen,step,step,reopen,step,...Array.from({length:5},()=>({action:'publish'}))]);
for(const version of Object.keys(definitions))add('catalog-read-and-full-fallback-'+version,[...rounds(4),reopen,step,terminal],{version,script:[page(),{reply:{error:{code:-32601,message:'Original unsupported items'}}},{reply:{result:{data:[{id:'original-turn',status:'completed',itemsView:'full',items:[{id:'answer',type:'agentMessage',text:'Full result '+version}]}],nextCursor:null}}},page()]});
add('in-progress-polls-are-not-retained',[step,reopen,step,terminal,step,step,step],{script:[page('inProgress'),...success]});
add('empty-original-result',[...rounds(4),reopen,step],{script:[page(),{reply:{result:{data:[],nextCursor:null}}},page()]});
for(const kind of ['native-error','gap','terminal-changed']){
 const failure=kind==='native-error'?{reply:{error:{code:-32000,message:'Original native read failure'}}}:kind==='gap'?{reply:{result:{data:[],nextCursor:null}}}:page('interrupted');
 add('durable-'+kind+'-and-bounded-retry',[...(kind==='terminal-changed'?rounds(2):[]),step,terminal,reopen,step,{action:'advance',millis:6000},step,terminal,...rounds(5)],{script:[...(kind==='terminal-changed'?[page(),item()]:[]),failure,...success]});
}
add('native-error-exhausts-third-attempt',[step,{action:'advance',millis:2000},step,{action:'advance',millis:4000},step,{action:'advance',millis:100000},step],{script:Array.from({length:3},()=>({reply:{error:{code:-32000,message:'Original native error'}}}))});
add('transport-loss-keeps-checkpoint',[control({readError:'fixture_unavailable'}),step,reopen,control({readError:null}),...rounds(4)]);
add('read-identity-refusal-is-not-semantic-failure',[step,...rounds(2)],{script:[{...page(),patch:{callerPrincipalId:'foreign'}},...success]});
add('epoch-restart-retains-predecessor',[step,control({epoch:'replacement-epoch'}),step,reopen,terminal,...rounds(3)],{script:[page(),...success]});
add('epoch-restart-terminal-only-in-predecessor',[step,control({epoch:'replacement-epoch'}),step,reopen,terminal,...rounds(4)],{script:[page(),page('inProgress'),...success]});
add('operator-retry-is-exact-and-idempotent',[step,{action:'retry',remember:true},{action:'retry',remember:false},reopen,{action:'retry',remember:false},...rounds(4)],{script:[{reply:{result:{data:[],nextCursor:null}}},...success]});
add('operator-stale-retry-refused',[step,{action:'retry',revision:999}],{script:[{reply:{result:{data:[],nextCursor:null}}}]});
add('operator-caller-scope-before-owner',[{action:'retry',caller:'foreign'}]);
add('retry-requires-failed-state',[step,{action:'retry'}]);
for(const [label,value]of [['wrong-turn',{turnId:'foreign'}],['wrong-call',{nativeCalls:{resume:null,turn:{objectId:'missing',revision:1},interrupt:null}}],['unstarted',{turnId:null}]])add(label,[patch('input',value),step]);
add('no-current-head',[control({noHead:true}),step,control({noHead:false}),step]);
add('duplicate-evidence-is-durable-gap',[step,patch('collection',{duplicateReads:true}),reopen,step]);
add('wrong-native-byte-total-is-durable-gap',[step,patch('collection',{nativeBytes:1}),reopen,step]);
add('input-cancellation-is-preserved-on-failure',[patch('input',{cancellation:{operationId:'cancel',callerPrincipalId:'alice',requestedAt:at,reason:'Original cancellation'}}),step],{script:[{reply:{result:{data:[],nextCursor:null}}}]});
add('no-exposure-over-terminal-input',[patch('input',{finishedAt:at}),step],{script:[{reply:{result:{data:[],nextCursor:null}}}]});
const huge='x'.repeat(1048575)+'😀'+'y'.repeat(2049);
add('actual-scalar-safe-result-parts',[...rounds(4),reopen,step],{script:[page(),item(huge),page()]});
for(const kind of ['before','after','contention'])for(const key of ['chat-bridge/result-text','chat-bridge/result'])add('result-'+key.split('/')[1]+'-'+kind,[...rounds(3),{action:'fault',value:{kind,key}},step,reopen,step,step,step]);
add('recreate-client-does-not-invent-mutation-receipts',[...rounds(3),{action:'fault',value:{kind:'after',key:'chat-bridge/result-text'}},step,{action:'new-client'},step]);
add('local-evidence-commit-before-collection-crash',[step,{action:'crash-collection'},step,reopen,step,...rounds(4)],{script:[page(),item(),item(),page()]});
add('local-complete-commit-crash',[...rounds(3),{action:'crash-complete'},step,reopen,step]);
const recovery={
 'local-evidence-commit-before-collection-crash':[terminal,...rounds(4),reopen,step],
 'local-complete-commit-crash':[terminal,step,reopen,step],
 'actual-scalar-safe-result-parts':[terminal,step,reopen,step],
 'durable-native-error-and-bounded-retry':[step,{action:'advance',millis:6000},...rounds(4),reopen,step],
 'epoch-restart-terminal-only-in-predecessor':[terminal,...rounds(4),reopen,step],
};
const runDirectly=process.argv[1]&&import.meta.url===pathToFileURL(process.argv[1]).href;
export {Client,compact};
if(runDirectly)for(const c of cases){
 const seed=definitions[c.seed??c.version],root=mkdtempSync(join(tmpdir(),'ivy-collector-')),file=join(root,'chat-bridge.sqlite');writeFileSync(file,Buffer.from(seed.database,'base64'));clock=seed.initialNow??now;
 let client=new Client(seed.records),store,admission,operations,queue,evidence,main,collector,publisher,readIndex=seed.initialReadIndex??0,last=0,retryRequest=null;
 const controls=clone(seed.initialControls??{epoch:'original-epoch'}),events=[],gates=[],aliases=new Map(),mutations=new Map();c.expected=[];c.observations=[];
 globalThis[probe]=(kind,input,fn)=>{const call={kind,input:(()=>{const v=clone(input);if(v.value)v.value=summary(v.value);if(v.params)v.params=summary(v.params);if(v.observation)v.observation=summary(v.observation);return v;})()};gates.push(call);try{const output=fn();if(output?.then)return output.then(value=>{call.output={value:value??null};return value;},e=>{call.output={error:error(e)};throw e;});call.output={value:output??null};return output;}catch(e){call.output={error:error(e)};throw e;}};
 function open(){
  store=new ChatStore(client,null,root);admission=new ChatAdmission(seed.definition);operations=new ChatOperations(store,admission);queue=new ChatQueue(operations);evidence=new ChatNativeEvidence(store,seed.definition);
  main={store,admission,operations,queue,async recoverPending(){events.push({method:'recover'});},async fixtureHead(id){events.push({method:'head',inputId:id??null});if(controls.noHead)return null;const current=await queue.main();if(!current.value.queue.length)return null;return{input:await store.read('chat-bridge/input',id??'original-input'),binding:await store.read('chat-bridge/binding',seed.binding)};},native:{evidence,async get(id,pin){events.push({method:'call',inputId:id,pin});return store.read('chat-bridge/native-call',pin,id);},async ready(){events.push({method:'ready'});return{epoch:controls.epoch,observedAt:new Date().toISOString()};},async verifyOwner(){events.push({method:'verify'});},async management(method,request){events.push({method:'read',request});if(controls.readError)throw new IvyError(controls.readError,'Original read transport is unavailable.');const spec=(c.script??success)[readIndex]??success[0],catalog=checkedNativeContract(c.version),observation={schemaVersion:1,observationId:'collection-observation-'+readIndex,serviceNodeId:'owner',callerPrincipalId:'chat-service',nativeVersion:c.version,epoch:controls.epoch,nativeExecutableHash:catalog.catalog.nativeExecutableHash,catalogHash:catalog.catalogHash,method:request.method,params:request.params,requestHash:hashJson({method:request.method,params:request.params}),requestId:'collection-request-'+readIndex,observedAt:new Date().toISOString(),reply:clone(spec.reply),...clone(spec.patch??{})};readIndex++;c.observations.push(observation);return observation;}}};
  collector=new ChatCollector(main);publisher=new ChatPublisher(collector);
 }
 function normalize(v){
  if(typeof v==='string'){if(aliases.has(v))return aliases.get(v);for(const [id,name]of aliases)if(v.includes(id))v=v.replaceAll(id,name);return v;}
  if(Array.isArray(v))return v.map(normalize);if(v&&typeof v==='object')return Object.fromEntries(Object.entries(v).map(([k,v])=>[k,normalize(v)]));return v;
 }
 function snapshot(output){
  const db=new DatabaseSync(file),rows=db.prepare('SELECT d.id,d.key,d.name,d.parent_id,d.current_revision,r.value_json FROM documents d JOIN revisions r ON d.id=r.id AND d.current_revision=r.revision ORDER BY d.name,d.id').all(),blobs=db.prepare('SELECT id,key,name,parent_id,bytes FROM blobs ORDER BY name,id').all();
  for(const r of rows)aliases.set(r.id,r.key==='chat-bridge/collection'?'<collection>':r.key==='chat-bridge/result'?'<result>':r.key==='chat-bridge/reply'?'<reply:'+r.name.split(' part ').at(-1)+'>':'<local:'+r.name+'>');
  for(const b of blobs)aliases.set(b.id,'<blob:'+b.name+'>');
  for(const call of client.calls)if(call.params.mutationId){const id=call.params.mutationId;if(!mutations.has(id))mutations.set(id,'<mutation:'+mutations.size+'>');aliases.set(id,mutations.get(id));}
  const writes=db.prepare('SELECT rowid,key,id,revision,value_json FROM collector_test_writes WHERE rowid>? ORDER BY rowid').all(last);last=writes.at(-1)?.rowid??last;db.close();
  const answer=compact(normalize({output,documents:rows.map(r=>({id:r.id,key:r.key,name:r.name,parent:r.parent_id,revision:r.current_revision,value:JSON.parse(r.value_json)})),blobs:blobs.map(b=>({id:b.id,key:b.key,name:b.name,parent:b.parent_id,byteLength:b.bytes.length,contentHash:digest(b.bytes)})),writes:writes.map(w=>({key:w.key,id:w.id,revision:w.revision,value:JSON.parse(w.value_json)})),records:client.records,calls:client.calls,events,gates}));
  client.calls.length=0;events.length=0;gates.length=0;return answer;
 }
 open();try{
  for(const s of c.steps){const output=await result(async()=>{
   switch(s.action){
    case'step':{const v=await collector.step('original-input');return v?{pin:v.pin,value:v.value}:null;}
    case'terminal':return collector.observedTerminal('original-input');
    case'publish':await publisher.step();return;
    case'reopen':store.close();open();return;
    case'new-client':store.close();client=new Client(client.records);open();return;
    case'control':Object.assign(controls,s.value);return;
    case'advance':clock+=s.millis;return;
    case'fault':client.fault=clone(s.value);return;
    case'patch':{
     if(s.target==='input'){Object.assign(client.records[0].content.value,clone(s.value));client.versions.get('original-input').set(client.records[0].revision.revision,clone(client.records[0]));}
     else{const current=await collector.find('original-input'),value={...current.value,...s.value};if(s.value.duplicateReads){delete value.duplicateReads;value.reads.push(value.reads[0]);}const db=new DatabaseSync(file);db.prepare('UPDATE revisions SET value_json=? WHERE id=? AND revision=?').run(JSON.stringify(value),current.pin.objectId,current.pin.revision);db.close();}return;
    }
    case'retry':{
     const current=await collector.find('original-input');if(!retryRequest||s.remember!==false)retryRequest={inputId:'original-input',expectedCollection:s.revision?{objectId:current.pin.objectId,revision:s.revision}:current?.pin??{objectId:'missing',revision:1},expectedBridge:admission.expected('alice'),operationId:`chat:${now}:retry`,reason:'Original operator retry'};
     return collector.retry(s.caller??'alice',retryRequest);
    }
    case'crash-collection':case'crash-complete':{
     const condition=s.action==='crash-complete'?"json_extract(NEW.value_json,'$.state')='complete'":"json_array_length(NEW.value_json,'$.reads')>1";
     const db=new DatabaseSync(file);db.exec(`CREATE TABLE collector_crash_arm(x INTEGER);INSERT INTO collector_crash_arm VALUES(1);CREATE TRIGGER collector_commit_crash BEFORE INSERT ON revisions WHEN EXISTS(SELECT 1 FROM documents WHERE id=NEW.id AND key='chat-bridge/collection') AND ${condition} AND EXISTS(SELECT 1 FROM collector_crash_arm) BEGIN SELECT RAISE(FAIL,'Original collector checkpoint unavailable'); END;`);db.close();return;
    }
    default:throw Error('Unknown action '+s.action);
   }
  });c.expected.push(snapshot(output));if(s.action==='reopen'){
   const db=new DatabaseSync(file);db.exec('DROP TRIGGER IF EXISTS collector_commit_crash;DROP TABLE IF EXISTS collector_crash_arm;');db.close();
   if(recovery[c.label]&&!c.recoverySeed){
    // Close SQLite to checkpoint the exact original database, then cross-open
    // the published copy with a fresh RPC client and no inherited receipts.
    store.close();const key='original-checkpoint-'+c.label;c.recoverySeed=key;
    definitions[key]={...clone(seed),database:readFileSync(file).toString('base64'),records:clone(client.records),initialNow:clock,initialControls:clone(controls),initialReadIndex:readIndex,snapshot:{sourceCase:c.label,afterStep:c.expected.length-1,contentHash:digest(readFileSync(file))}};
    cases.push({label:'cross-open-'+c.label,version:c.version,seed:key,script:clone(c.script??success),steps:recovery[c.label]});open();
   }
  }}
  if(c.label==='collect-reopen-publish-and-release'&&(c.expected.some(s=>s.output.error)||c.expected.at(-1).records[0].content.value.state!=='completed'))throw Error('Original full integration did not succeed: '+JSON.stringify(c.expected.map(s=>s.output)));
  if(c.label==='actual-scalar-safe-result-parts'){const parts=client.records.filter(r=>r.object.contractKey==='chat-bridge/result-text');if(parts.length!==2||Buffer.byteLength(parts[0].content.value)!==1048575||parts.some(r=>r.content.value.includes('\ufffd')))throw Error('Real scalar-safe split not exercised');}
  if(c.label.startsWith('cross-open-')){
   if(c.expected.some(s=>s.output.error))throw Error('Original cross-open failed '+c.label+': '+JSON.stringify(c.expected.map(s=>s.output)));
   const current=c.expected.at(-1).documents.find(d=>d.key==='chat-bridge/collection');
   if(current.value.state!==(c.label==='cross-open-local-complete-commit-crash'?'ready':'complete'))throw Error('Original cross-open checkpoint differs '+c.label);
   if(c.expected[0].output.value!==true&&c.label!=='cross-open-durable-native-error-and-bounded-retry')throw Error('Original terminal history was not reconstructed '+c.label);
  }
 }finally{store.close();rmSync(root,{recursive:true,force:true});}
}
if(runDirectly)process.stdout.write(JSON.stringify({now,instructions,seeds:definitions,cases}));
