import './chat-domain-loader.mjs';
import {registerHooks} from 'node:module';
import {existsSync,mkdtempSync,readFileSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,basename} from 'node:path';
import {pathToFileURL} from 'node:url';
import {DatabaseSync} from 'node:sqlite';
import {buildInstructions} from '../../../tools/build/build-instructions.mjs';
const url=path=>new URL(path,import.meta.url).href,data=source=>'data:text/javascript;base64,'+Buffer.from(source).toString('base64');
const probe=Symbol.for('ivy.test.chat-native-evidence-gate'),raw=Symbol.for('ivy.test.chat-native-evidence-contracts');globalThis[raw]=new WeakMap();
const plans=data(`import{validateChatNativeDraft as draft,resolveChatPlan as resolve,chatTurnOptions}from'${url('../../../services/chat-bridge/src/native-plan.ts')}';
 export{chatTurnOptions};export function validateChatNativeDraft(value){return globalThis[Symbol.for('ivy.test.chat-native-evidence-gate')]('draft',{value},()=>draft(value));}
 export function resolveChatPlan(store,plan){return globalThis[Symbol.for('ivy.test.chat-native-evidence-gate')]('plan',{plan},()=>resolve(store,plan));}`);
const sdk=data(`export{canonical,hashJson}from'${url('../../../packages/contracts/src/canonical.ts')}';
 export{requireThat}from'${url('../../../packages/contracts/src/errors.ts')}';export{validateChat}from'${url('../../../packages/contracts/src/chat-validation.ts')}';
 import{checkedNativeContract as checked}from'${url('../../../packages/contracts/src/checked-native-contract.ts')}';
 export function checkedNativeContract(...args){const selected=checked(...args),target=selected.contract,contract=new Proxy(target,{get(t,k){
  if(k==='validateInput')return(method,params,maximumBytes)=>globalThis[Symbol.for('ivy.test.chat-native-evidence-gate')]('input',{contract:{version:t.catalog.version,sourceHash:t.catalog.sourceHash,catalogHash:t.catalogHash},method,params,maximumBytes},()=>t.validateInput(method,params,maximumBytes));
  const v=Reflect.get(t,k,t);return typeof v==='function'?v.bind(t):v;}});globalThis[Symbol.for('ivy.test.chat-native-evidence-contracts')].set(contract,target);return{...selected,contract};}`);
const payload=data(`export{validateNativeOperation,validateNativeRead}from'${url('../../../packages/sdk/src/native-evidence.ts')}';
 import{validateNativePayload as validate}from'${url('../../../packages/sdk/src/native-evidence.ts')}';
 export function validateNativePayload(contract,observation,limits){const t=globalThis[Symbol.for('ivy.test.chat-native-evidence-contracts')].get(contract)??contract;
 return globalThis[Symbol.for('ivy.test.chat-native-evidence-gate')]('payload',{contract:{version:t.catalog.version,sourceHash:t.catalog.sourceHash,catalogHash:t.catalogHash},observation,limits},()=>validate(t,observation,limits));}`);
registerHooks({resolve(s,c,n){
 if(['/native-evidence.ts','/evidence-bytes.ts'].some(p=>c.parentURL?.endsWith('/chat-bridge/src'+p))&&s==='./native-plan.js')return{url:plans,shortCircuit:true};
 if(c.parentURL?.endsWith('/chat-bridge/src/native-evidence.ts')&&s==='../../../packages/sdk/src/node.js')return{url:sdk,shortCircuit:true};
 if(c.parentURL?.endsWith('/chat-bridge/src/native-evidence.ts')&&s==='../../../packages/sdk/src/native-evidence.js')return{url:payload,shortCircuit:true};return n(s,c);
}});
const {ChatStore}=await import('../../../services/chat-bridge/src/store.ts');
const {ChatAdmission}=await import('../../../services/chat-bridge/src/admission.ts');
const {ChatOperations}=await import('../../../services/chat-bridge/src/operations.ts');
const {ChatNativeEvidence,verifyChatNativeRequest,verifyChatNativeOperation,verifyChatNativeRead}=await import('../../../services/chat-bridge/src/native-evidence.ts');
const {saveChatEvidence}=await import('../../../services/chat-bridge/src/evidence-bytes.ts');
const {resolveChatPlan,chatTurnOptions}=await import('../../../services/chat-bridge/src/native-plan.ts');
const {mainTurnContext,mainInputOrigin}=await import('../../../services/chat-bridge/src/main-context.ts');
const {checkedNativeContract,nativeCatalogPath}=await import('../../../packages/contracts/src/checked-native-contract.ts');
const {nativeVersions}=await import('../../../packages/contracts/src/native-versions.ts');
// Public schemas may admit a newer version before its offline catalog is
// retained. Build comparison seeds only for actual checked-in catalog inputs.
const retainedVersions=nativeVersions.filter(version=>existsSync(nativeCatalogPath(process.cwd(),version)));
const {unpersistedNativePlan}=await import('../../fixtures/native-plan.ts');
const {canonical,digest,hashJson}=await import('../../../packages/contracts/src/canonical.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const now=1790000000123,at=new Date(now).toISOString(),RealDate=Date,maximum=32*1024*1024;
globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[now]));}static now(){return now;}};
const summary=value=>{const encoded=canonical(value,maximum);return{byteLength:Buffer.byteLength(encoded),contentHash:digest(encoded),...(encoded.length<512?{value}:{} )};};
const error=e=>{e=IvyError.from(e);return{code:e.code,message:e.message,outcome:e.outcome};};
const result=async fn=>{try{return{value:(await fn())??null};}catch(e){return{error:error(e)};}};
const assetRoot=mkdtempSync(join(tmpdir(),'ivy-native-evidence-assets-'));let instructions;
try{const files=buildInstructions({root:process.cwd(),outputRoot:assetRoot,component:'chat-bridge'});instructions=readFileSync(files.find(p=>basename(p)==='chat-main-instructions.json')).toString('base64');}finally{rmSync(assetRoot,{recursive:true,force:true});}
const channel={adapter:'whatsapp',accountId:'account',channelId:'channel'},seeds={};
for(const version of retainedVersions){
 const root=mkdtempSync(join(tmpdir(),'ivy-native-evidence-seed-')),plan=unpersistedNativePlan(version);plan.threadResume.reservedFields=['history','path','threadId'];plan.turnStart.reservedFields=['clientUserMessageId','input','threadId'];
 const definition={workspaceId:'chat',principalId:'chat-service',rootObjectId:null,project:{serviceNodeId:'owner',namespace:'codex',kind:'project',nativeId:'/synthetic/project'},nativePlan:plan,channels:[{channel,displayName:'Channel'}]},admission=new ChatAdmission(definition);
 const store=new ChatStore({async request(){throw Error('Unexpected seed RPC.');}},null,root);store.setRuntimeEpoch('chat');const ops=new ChatOperations(store,admission);
 try{
  const send=await ops.begin('alice',{action:'send',operationId:`chat:${now}:send`,expectedBridge:admission.expected('alice'),channel,messageId:'original-message',expectedBinding:null,payload:{text:'Original input',images:[]}});
  const notice=await ops.begin('producer',{action:'notify',operationId:`chat:${now}:notice`,expectedBridge:admission.expected('producer','notice'),channel,source:{objectId:'source',revision:1},text:'Original notice'});
  const value={schemaVersion:1,workspaceId:'chat',definitionHash:admission.definitionHash,identity:{channel,senderPrincipalId:'alice',messageId:'original-message'},operationId:send.value.operationId,requestHash:digest('input'),binding:{objectId:'binding',revision:1},sequence:1,payload:{text:'Original input',images:[]},state:'running',nativeCalls:{resume:null,turn:null,interrupt:null},turnId:'original-turn',epoch:'original-epoch',result:null,error:null,cancellation:null,admittedAt:at,updatedAt:at,finishedAt:null};
  const input={object:{id:'original-input',parentId:null,name:'Original input',contractKey:'chat-bridge/input',effectivelyArchived:false},revision:{revision:1,contractVersion:'1.3.0',mediaType:'application/json'},content:{encoding:'json',value}};
  store.close();
  const db=new DatabaseSync(join(root,'chat-bridge.sqlite'));db.exec(`CREATE TABLE native_evidence_test_writes(value_json TEXT);CREATE TRIGGER native_evidence_test_revision AFTER INSERT ON revisions WHEN EXISTS(SELECT 1 FROM documents WHERE id=NEW.id AND key='chat-bridge/evidence') BEGIN INSERT INTO native_evidence_test_writes VALUES(NEW.value_json);END;`);db.close();
  seeds[version]={definition,input,send:send.pin,notice:notice.pin,database:readFileSync(join(root,'chat-bridge.sqlite')).toString('base64')};
 }finally{rmSync(root,{recursive:true,force:true});}
}
const methodKeys={'thread/start':'threadStart','thread/resume':'threadResume','turn/start':'turnStart','turn/interrupt':'turnInterrupt'};
async function bodies(store,definition,input,spec={}){
 const source=checkedNativeContract(definition.nativePlan.nativeVersion),plan=await resolveChatPlan(store,definition.nativePlan),method=spec.method??'turn/start',key=methodKeys[method];
 const origin=spec.notice?mainInputOrigin('producer',{action:'notify',source:{objectId:'source',revision:1}}):'conversation input';
 let params=method==='turn/interrupt'?{threadId:'original-thread',turnId:'original-turn'}:method==='turn/start'?{...mainTurnContext(chatTurnOptions(plan.turnStart,input.content.value.payload.turnOptions),origin),threadId:'original-thread',input:[{type:'text',text:'Original input\n😀',text_elements:[]}]}:{...plan[key],...(method==='thread/resume'?{threadId:'original-thread'}:{})};
 if(spec.excludeTurns!==undefined)params.excludeTurns=spec.excludeTurns;
 if(spec.large)params.input=[{type:'image',url:'data:image/png;base64,'+Buffer.alloc(4*1024*1024,31).toString('base64')}];
 if(spec.envelopeBytes){params.input[0].text='';const base=Buffer.byteLength(canonical({id:'00000000-0000-0000-0000-000000000000',method,params}));params.input[0].text='x'.repeat(spec.envelopeBytes-base);}
 const request={nativeVersion:definition.nativePlan.nativeVersion,catalogSourceHash:source.catalog.sourceHash,method,params};
 const call={schemaVersion:1,operationId:'original-native-call',serviceNodeId:'owner',callerPrincipalId:'chat-service',request:{objectId:'original-input',revision:1},nativeVersion:request.nativeVersion,catalogSourceHash:request.catalogSourceHash,method,predecessor:null,preparedEpoch:'original-epoch',requestHash:hashJson({method,params}),expectedDefinitionHash:definition.nativePlan.definitions[key],createdAt:at,state:'prepared',epoch:null,evidence:null,code:null,updatedAt:at};
 const operation={schemaVersion:1,operationId:call.operationId,callerPrincipalId:call.callerPrincipalId,serviceNodeId:call.serviceNodeId,nativeVersion:request.nativeVersion,nativeExecutableHash:source.catalog.nativeExecutableHash,method,params,requestHash:call.requestHash,phase:'succeeded',createdAt:'2026-09-07T08:59:59.000Z',updatedAt:'2026-09-07T08:59:59.500Z',epoch:'original-epoch',requestId:'original-request-id',code:null,reply:{result:{turn:{id:'original-turn',status:'completed',items:[{id:'original-message',type:'agentMessage',text:spec.large?'r'.repeat(5*1024*1024):'Original saved reply.\n😀'}]}}}};
 const expected={serviceNodeId:'owner',callerPrincipalId:'chat-service',nativeVersion:request.nativeVersion,epoch:'original-epoch',method:'thread/read',params:{threadId:'original-thread',includeTurns:true}};
 const read={schemaVersion:1,observationId:'original-read',...expected,nativeExecutableHash:source.catalog.nativeExecutableHash,catalogHash:source.catalogHash,requestId:'original-read-request',observedAt:'2026-09-07T09:00:01.000Z',requestHash:hashJson({method:expected.method,params:expected.params}),reply:{error:{code:-32601,message:'Original unsupported read.\n\ud800'}}};
 return{request,call,operation,expected,read};
}
const cases=[],add=(label,steps,options={})=>cases.push({label,version:'0.154.0',steps,...options});
const saveRequest={action:'save-request'},loadRequest={action:'request'},saveOperation={action:'save-operation'},loadOperation={action:'operation'},saveRead={action:'save-read'},loadRead={action:'read'},reopen={action:'reopen'};
const patch=(target,value)=>({action:'patch',target,value});
for(const version of retainedVersions){add('catalog-'+version+'-read',[{action:'verify-read'},saveRead,reopen,loadRead],{version});add('catalog-'+version+'-interrupt',[{action:'verify-request'},saveRequest,loadRequest],{version,spec:{method:'turn/interrupt'}});}
for(const method of ['thread/start','thread/resume','turn/start','turn/interrupt'])add('request-'+method,[{action:'verify-request'},saveRequest,reopen,loadRequest],{spec:{method}});
add('resume-projection-preserved',[{action:'verify-request'}],{spec:{method:'thread/resume',excludeTurns:true}});
add('turn-notice-context',[{action:'verify-request'},saveRequest,saveOperation,reopen,loadOperation],{spec:{notice:true}});
add('turn-input-preferences',[{action:'verify-request'},saveRequest,saveOperation,loadOperation],{inputPatch:{payload:{text:'Original input',images:[],turnOptions:{model:'selected-model',effort:'low'}}}});
add('complete-authority-and-bytes',[{action:'verify-operation'},saveRequest,loadRequest,saveOperation,loadOperation,saveOperation,reopen,loadOperation,saveRead,loadRead]);
add('large-original-request-operation-reopen',[saveRequest,saveOperation,reopen,loadOperation],{spec:{large:true}});
for(const n of [-1,0,1])add('actual-request-envelope-'+n,[{action:'verify-request'}],{spec:{envelopeBytes:6291456+n}});
for(const [key,value]of [['callerPrincipalId','foreign'],['serviceNodeId','foreign'],['operationId','foreign'],['nativeExecutableHash',digest('different')],['requestHash',digest('different')],['params',{threadId:'other',input:[]}],['nativeVersion','0.0.0'],['updatedAt','2026-09-07T08:59:58.000Z']])add('operation-'+key,[patch('operation',{[key]:value}),{action:'verify-operation'}]);
for(const [key,value]of [['epoch','foreign'],['catalogHash',digest('different')],['callerPrincipalId','foreign'],['serviceNodeId','foreign'],['nativeExecutableHash',digest('different')],['requestHash',digest('different')],['params',{threadId:'other'}],['observedAt','2026-09-07T09:00:61.000Z']])add('read-'+key,[patch('read',{[key]:value}),{action:'verify-read'},saveRead]);
for(const [key,value]of [['callerPrincipalId','foreign'],['serviceNodeId','foreign'],['requestHash',digest('different')],['expectedDefinitionHash',digest('different')],['method','thread/start'],['catalogSourceHash',digest('different')]])add('call-'+key,[patch('call',{[key]:value}),{action:'verify-request'}]);
add('different-native-settings',[{action:'change-settings'},{action:'verify-request'}]);
add('changed-retained-plan',[{action:'change-plan-identity'},{action:'verify-request'}]);
add('invalid-call-before-shared-gates',[patch('call',{preparedEpoch:4}),{action:'verify-request'}]);
add('stat-original-id-selection',[{action:'verify-request'}],{statPatch:{id:'different-input'}});
add('stat-original-key-precedence',[{action:'verify-request'}],{statPatch:{key:'different-contract'}});
add('malformed-draft-first',[patch('request',{params:{threadId:4}}),{action:'verify-request'}]);
add('forged-result-payload',[patch('operation',{reply:{result:{invented:true}}}),{action:'verify-operation'}]);
add('forged-read-payload',[patch('read',{reply:{result:{fabricated:'thread'}}}),{action:'verify-read'}]);
add('authority-before-save-operation',[saveRequest,patch('operation',{callerPrincipalId:'foreign'}),saveOperation,{action:'raw-operation'},loadOperation]);
add('authority-before-save-read',[patch('read',{epoch:'foreign'}),saveRead,{action:'raw-read'},loadRead]);
for(const phase of ['accepted','dispatched','outcome_unknown','failed'])add('retained-operation-'+phase,[saveRequest,patch('operation',{phase,...(phase==='accepted'?{epoch:null,requestId:null,reply:null,code:null}:phase==='outcome_unknown'?{reply:null,code:'native_epoch_lost'}:phase==='dispatched'?{reply:null,code:null}:{reply:{error:{code:-1,message:'Original native error'}},code:'native_error'})}),saveOperation,reopen,loadOperation]);
for(const key of ['workspaceId','definitionHash'])add('scope-'+key,[saveRequest,patch('input',{[key]:'foreign'}),saveOperation,loadRequest,saveRead]);
add('configured-read-owner-before-scope',[{action:'raw-read'},patch('expected',{serviceNodeId:'foreign'}),saveRead,loadRead]);
add('missing-parent-before-request-bytes',[saveRequest],{parent:'missing'});
add('local-operation-parent',[saveRequest,reopen,loadRequest],{parent:'operation',spec:{method:'turn/interrupt'}});
add('owner-read-outage',[{action:'remote-error'},saveRequest,saveRead]);
add('constructor-root-before-effects',[{action:'construct-other-root'}]);
add('copied-definition-remains-pinned',[{action:'mutate-definition'},saveRequest,loadRequest],{spec:{method:'turn/interrupt'}});

async function execute(c){
 const seed=seeds[c.version],root=mkdtempSync(join(tmpdir(),'ivy-native-evidence-')),file=join(root,'chat-bridge.sqlite');writeFileSync(file,Buffer.from(seed.database,'base64'));
 const definition=structuredClone(seed.definition),input=structuredClone(seed.input),calls=[],gates=[],pins=new Map(),aliases=new Map();let remoteError=false,store;
 if(c.inputPatch)Object.assign(input.content.value,structuredClone(c.inputPatch));if(c.spec?.notice){input.content.value.identity.senderPrincipalId='producer';input.content.value.operationId=`chat:${now}:notice`;input.content.value.identity.messageId=input.content.value.operationId;}
 const client={async request(method,p){calls.push({method,params:structuredClone(p)});if(remoteError)throw new IvyError('fixture_owner_unavailable','Original owner read unavailable.','unknown');if(p.objectId!=='original-input')throw new IvyError('object_missing','Original parent is unavailable.');if(method==='objects.read')return structuredClone(input);if(method==='objects.stat')return{...structuredClone(input.object),...c.statPatch};throw Error('Unexpected RPC '+method);}};
 store=new ChatStore(client,null,root);let body=await bodies(store,definition,input,c.spec),evidence=new ChatNativeEvidence(store,definition),lastWrite=0;
 c.body=structuredClone(body);
 if(c.spec?.large){c.body.request.params.input[0].url='';c.body.operation.params.input[0].url='';c.body.operation.reply.result.turn.items[0].text='';}
 if(c.spec?.envelopeBytes){c.body.request.params.input[0].text='';c.body.operation.params.input[0].text='';}
 const parent=()=>c.parent==='operation'?seed.send.objectId:c.parent==='missing'?'missing':'original-input';
 const pinFor=name=>{const pin=pins.get(name);if(!pin)throw new IvyError('fixture_pin_missing','Original evidence pin is unavailable.');return pin;};
 const selectedCall=()=>({...body.call,request:pins.get('request')??body.call.request});
 globalThis[probe]=(kind,value,fn)=>{
  const call={kind,input:Object.fromEntries(Object.entries(value).map(([k,v])=>[k,['value','params','observation'].includes(k)?summary(v):structuredClone(v)]))};gates.push(call);
  const succeeded=v=>{call.output={value:kind==='plan'?v:null};return v;},failed=e=>{call.output={error:error(e)};throw e;};
  try{const r=fn();return r?.then?r.then(succeeded,failed):succeeded(r);}catch(e){return failed(e);}
 };
 const snapshot=output=>{
  const db=new DatabaseSync(file),docs=db.prepare("SELECT d.id,d.name,d.parent_id,d.current_revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key='chat-bridge/evidence' ORDER BY d.name").all(),blobs=db.prepare('SELECT id,key,name,parent_id,bytes FROM blobs ORDER BY name,parent_id').all();
  for(const r of [...docs,...blobs])aliases.set(r.id,'<local:'+r.name+'>');
  const writes=db.prepare('SELECT rowid,value_json FROM native_evidence_test_writes WHERE rowid>? ORDER BY rowid').all(lastWrite);lastWrite=writes.at(-1)?.rowid??lastWrite;db.close();
  const normalize=v=>typeof v==='string'?(aliases.get(v)??v):Array.isArray(v)?v.map(normalize):v&&typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,v])=>[k,normalize(v)])):v;
  const saved=normalize({output,gates,calls,documents:docs.map(r=>({id:r.id,name:r.name,parentId:r.parent_id,revision:r.current_revision,value:JSON.parse(r.value_json)})),blobs:blobs.map(r=>({id:r.id,key:r.key,name:r.name,parentId:r.parent_id,byteLength:r.bytes.length,contentHash:digest(r.bytes)})),writes:writes.map(r=>JSON.parse(r.value_json))});gates.length=0;calls.length=0;return saved;
 };
 c.expected=[];calls.length=0;
 try{for(const s of c.steps){let output=await result(async()=>{switch(s.action){
  case'patch':if(s.target==='input')Object.assign(input.content.value,structuredClone(s.value));else Object.assign(body[s.target],structuredClone(s.value));return;
  case'verify-request':return verifyChatNativeRequest(store,definition,body.call,body.request);
  case'verify-operation':return summary(await verifyChatNativeOperation(store,definition,body.call,body.request,body.operation));
  case'verify-read':return verifyChatNativeRead(body.read,body.expected,definition.nativePlan.catalogSourceHash);
  case'save-request':{const pin=await evidence.saveRequest(parent(),body.request);pins.set('request',pin);return pin;}
  case'request':return summary(await evidence.request(parent(),selectedCall()));
  case'save-operation':{const pin=await evidence.saveOperation(parent(),selectedCall(),body.operation);pins.set('operation',pin);return pin;}
  case'operation':return summary(await evidence.operation(parent(),selectedCall(),pinFor('operation')));
  case'save-read':{const pin=await evidence.saveRead(parent(),body.expected,body.read);pins.set('read',pin);return pin;}
  case'read':return summary(await evidence.read(parent(),body.expected,pinFor('read')));
  case'raw-operation':pins.set('operation',await saveChatEvidence(store,parent(),'agent-operation/canonical-json',body.operation));return;
  case'raw-read':pins.set('read',await saveChatEvidence(store,parent(),'agent-read-observation/canonical-json',body.read));return;
  case'reopen':store.close();store=new ChatStore(client,null,root);evidence=new ChatNativeEvidence(store,definition);return;
  case'remote-error':remoteError=true;return;
  case'change-settings':body.request.params.model='unplanned';body.call.requestHash=hashJson({method:body.request.method,params:body.request.params});return;
  case'change-plan-identity':definition.nativePlan.definitions.threadStart=digest('changed-plan-definition');return;
  case'construct-other-root':{const other=new ChatStore(client,'other');try{return new ChatNativeEvidence(other,definition);}finally{other.close();}}
  case'mutate-definition':definition.workspaceId='changed-after-construction';return;
  default:throw Error('Unknown action '+s.action);
 }});c.expected.push(snapshot(output));}}
 finally{store.close();rmSync(root,{recursive:true,force:true});}
}
export {now,RealDate,instructions,seeds,bodies,summary,error,result};
if(process.argv[1]&&import.meta.url===pathToFileURL(process.argv[1]).href){
 for(const c of cases)await execute(c);
 for(const label of ['complete-authority-and-bytes','large-original-request-operation-reopen','turn-notice-context','turn-input-preferences','local-operation-parent','copied-definition-remains-pinned']){const c=cases.find(c=>c.label===label);if(c.expected.some(e=>e.output.error))throw Error('Successful original path failed '+label+': '+JSON.stringify(c.expected.map(e=>e.output)));}
 process.stdout.write(JSON.stringify({now,instructions,seeds,cases}));
}
