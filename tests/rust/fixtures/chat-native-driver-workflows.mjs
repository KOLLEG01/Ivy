import {now,instructions,seeds,bodies,summary,error,result} from './chat-native-evidence-oracle.mjs';
import {mkdtempSync,readFileSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {DatabaseSync} from 'node:sqlite';
const {ChatStore,mutation}=await import('../../../services/chat-bridge/src/store.ts');
const {ChatAdmission}=await import('../../../services/chat-bridge/src/admission.ts');
const {ChatNativeDriver,nativePath}=await import('../../../services/chat-bridge/src/native-driver.ts');
const {checkedNativeContract}=await import('../../../packages/contracts/src/checked-native-contract.ts');
const {hashJson,digest}=await import('../../../packages/contracts/src/canonical.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');

// The native owner and Hive parent are explicit synthetic RPC boundaries. All
// driver, SDK reconciliation, plan/schema validation and SQLite code is original.
globalThis[Symbol.for('ivy.test.chat-native-evidence-gate')]=(_kind,_input,run)=>run();
const cases=[],add=(label,steps,options={})=>cases.push({label,version:'0.154.0',steps,...options});
const prepare={action:'prepare'},get={action:'get'},advance={action:'advance'},prevent={action:'prevent'},reopen={action:'reopen'};
const state=(value)=>({action:'state',value});
const patch=(target,value)=>({action:'patch',target,value});
for(const version of Object.keys(seeds)){
 add('prepare-recover-'+version,[prepare,get,reopen,get,prepare],{version});
 add('dispatch-retain-'+version,[prepare,advance,get,reopen,advance],{version});
}
for(const method of ['thread/start','thread/resume','turn/interrupt'])add('prepare-'+method,[prepare,get,reopen,prepare],{spec:{method}});
add('resume-new-projection',[{...prepare,projection:true},reopen,{...prepare,projection:true},get],{spec:{method:'thread/resume'}});
add('resume-retained-false-projection',[prepare,{...prepare,projection:true},get],{spec:{method:'thread/resume',excludeTurns:false}});
add('resume-retained-missing-projection',[prepare,{...prepare,projection:true},get],{spec:{method:'thread/resume'}});
add('same-id-original-origin',[prepare,patch('origin',{preparedEpoch:'other'}),prepare]);
add('same-id-original-predecessor',[prepare,patch('origin',{predecessor:{objectId:'foreign',revision:1}}),prepare]);
add('same-id-different-request',[prepare,patch('requestParams',{input:[{type:'text',text:'Changed input',text_elements:[]}]}),prepare]);
add('retained-settings-checked-before-bytes',[patch('requestParams',{model:'unplanned'}),prepare]);
add('steer-not-plan-method',[patch('request',{method:'turn/steer',params:{threadId:'original-thread',expectedTurnId:'original-turn',input:[]}}),prepare],{version:'0.159.2'});
add('owner-before-malformed-request',[patch('node',{connected:false}),patch('request',{params:{threadId:42}}),prepare]);
add('parent-scope-before-request-publication',[prepare],{parent:'missing'});
add('observed-progress-and-current-pin',[prepare,state({lookup:'accepted'}),advance,get,state({lookup:'dispatched'}),advance,{...get,current:true},state({lookup:'succeeded'}),advance,get]);
add('unchanged-observation-no-new-revision',[prepare,state({lookup:'accepted'}),advance,advance,reopen,advance]);
add('observed-absence-cannot-authorize-replay',[prepare,state({lookup:'accepted'}),advance,state({lookup:'absent'}),advance]);
add('absent-prepared-epoch-mismatch',[prepare,state({epoch:'new-epoch'}),advance]);
add('absent-resume-next-epoch',[prepare,state({epoch:'new-epoch',invoke:'accepted'}),advance],{spec:{method:'thread/resume'}});
add('unscoped-preparation-epoch',[prepare,state({epoch:'new-epoch'}),advance],{origin:{predecessor:null,preparedEpoch:null}});
add('guard-before-original-dispatch',[prepare,state({guardError:true}),advance]);
add('owner-rechecked-after-live-guard',[prepare,state({disconnectAtGuard:true}),advance]);
add('lost-invoke-reply-reconciles-same-id',[prepare,state({loseInvoke:true}),advance,get]);
add('lost-invoke-without-observed-receipt',[prepare,state({loseInvoke:true,afterInvoke:'absent'}),advance,get]);
add('generic-not-found-is-not-owner-absence',[prepare,state({lookup:'generic'}),advance]);
add('mismatched-owner-absence',[prepare,state({absenceOwner:'foreign'}),advance]);
add('owner-operation-identity-mismatch',[prepare,state({lookup:'succeeded',receiptPatch:{operationId:'foreign'}}),advance]);
add('owner-operation-payload-mismatch',[prepare,state({lookup:'succeeded',receiptPatch:{reply:{result:{invented:true}}}}),advance]);
add('owner-outcome-unknown-is-terminal',[prepare,state({lookup:'outcome_unknown'}),advance,state({lookup:'absent'}),advance]);
add('prevention-retains-owner-receipt',[prepare,prevent,reopen,get,prevent]);
add('prevention-race-keeps-accepted-operation',[prepare,state({lookup:'succeeded'}),prevent,get]);
add('prevention-unknown-cannot-dispatch',[prepare,state({preventError:true}),prevent,get]);
add('prevention-guard-before-effect',[prepare,state({guardError:true}),prevent]);
add('stale-envelope-rechecks-live-guard',[prepare,state({staleEnvelope:true}),advance]);
add('stale-envelope-second-guard-refuses',[prepare,state({staleEnvelope:true,secondGuardError:true}),advance]);
add('stale-envelope-cannot-change-native-definition',[prepare,state({staleEnvelope:true,changedNative:true}),advance]);
add('mutable-pin-read-follows-original-current-head',[prepare,{action:'rewrite',value:{preparedEpoch:'other'}},get,{...get,current:true}]);
add('canonical-name-before-request-read',[prepare,{action:'rename',name:'Foreign call'},get]);
add('observed-state-needs-original-evidence',[prepare,{action:'rewrite',value:{state:'accepted'}},get]);
add('invalid-call-schema-before-retained-receipt',[prepare,advance,{action:'rewrite',value:{code:'foreign'}},get]);
add('call-and-retained-receipt-must-agree',[prepare,advance,{action:'rewrite',value:{epoch:'foreign'}},get]);
add('original-receipt-cannot-be-mutated',[prepare,advance,{action:'corrupt-receipt'},get]);

for(const [label,start,roots,resume,turn]of[
 ['posix','/Synthetic/project/../project',['/Synthetic'],null,null],
 ['windows','D:\\Testing\\Project\\..\\Project\\',['d:/testing'],null,null],
 ['unc','\\\\Server\\Share\\Project\\',['\\\\server\\share'],null,null],
 ['extended','\\\\?\\D:\\Project',['\\\\?\\d:\\'],null,null],
 ['relative','relative',['/'],null,null],
 ['outside','/project-other',['/project'],null,null],
 ['drive-root','D:\\',['d:/'],null,null],
 ['resume-switch','/project',['/project'],'/other',null],
 ['turn-switch','/project',['/project'],null,'/other'],
 ['case-sensitive-posix','/Project',['/project'],null,null],
 ['null-resume','/project',['/project'],null,null],
 ['empty-resume','/project',['/project'],'',null],
 ['persistent','/project',['/project'],null,null],
])add('project-'+label,[{action:'verify-project'}],{project:{start,roots,resume,turn,...(label==='persistent'?{ephemeral:true}:{})}});
for(const spec of [{missing:true},{duplicate:true},{badProjects:true},{bindingHash:true},{bindingOwner:true}])add('project-'+Object.keys(spec)[0],[{action:'verify-project'}],{project:{start:'/project',roots:['/project'],...spec}});

async function execute(c){
 const seed=seeds[c.version],root=mkdtempSync(join(tmpdir(),'ivy-driver-original-')),file=join(root,'chat-bridge.sqlite');
 writeFileSync(file,Buffer.from(seed.database,'base64'));
 const definition=structuredClone(seed.definition),input=structuredClone(seed.input),events=[],aliases=new Map(),pins=new Map();
 if(c.project){const p=c.project;Object.assign(definition.nativePlan.threadStart.params,{cwd:p.start,...(p.ephemeral?{ephemeral:true}:{})});
  if(p.resume!==undefined)definition.nativePlan.threadResume.params.cwd=p.resume;
  if(p.turn!==undefined)definition.nativePlan.turnStart.params.cwd=p.turn;}
 const source=checkedNativeContract(c.version),node={principalId:'chat-service',serviceName:'chat-bridge',connected:true,synced:true};
 const status={serviceNodeId:'owner',hostId:'native-host',nativeVersion:c.version,nativeExecutableHash:source.catalog.nativeExecutableHash,catalogHash:source.catalogHash,epoch:'original-epoch',pid:42,state:'ready',observedAt:new Date(now).toISOString(),code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
 let state={lookup:'absent',invoke:'succeeded',epoch:'original-epoch'},store,driver,body,lastWrite=0,guardCount=0,envelopeRevision=1;
 const origin={predecessor:null,preparedEpoch:'original-epoch',...c.origin};
 const ownerReceipt=phase=>{
  const request=body.request;
  let value={...structuredClone(body.operation),operationId:'driver-original-call',method:request.method,params:structuredClone(request.params),requestHash:hashJson({method:request.method,params:request.params}),phase};
  if(phase==='accepted')Object.assign(value,{epoch:null,requestId:null,reply:null,code:null});
  else if(phase==='dispatched')Object.assign(value,{reply:null,code:null});
  else if(phase==='outcome_unknown')Object.assign(value,{reply:null,code:'native_epoch_lost'});
  else if(phase==='failed')Object.assign(value,{reply:{error:{code:-1,message:'Original prevention.'}},code:'native_error'});
  return Object.assign(value,structuredClone(state.receiptPatch??{}));
 };
 const rpc=async(method,p)=>{
  if(method==='objects.read'){if(p.objectId==='original-input')return structuredClone(input);throw new IvyError('object_missing','Original parent is unavailable.');}
  if(method==='objects.stat'){if(p.objectId==='original-input')return structuredClone(input.object);throw new IvyError('object_missing','Original parent is unavailable.');}
  if(method==='serviceNodes.get')return structuredClone(node);
  if(method==='tools.list'){
   const namespace=p.namespace??(p.namePrefix.startsWith('codex.')?'codex':'agent'),native=namespace==='codex',name=p.namePrefix.replace(/^(codex|agent)\./,'');
   const definition=native?source.definitions.get(name):{namespace:'agent',name,interfaceVersion:'1.0.0',inputSchema:{},outputSchema:{}};
   const definitionHash=native?(c.project?.bindingHash||state.changedNative&&envelopeRevision>1?digest('changed-native'):hashJson(definition)):digest('envelope-'+name+'-'+envelopeRevision);
   return{provider:{node:{serviceNodeId:c.project?.bindingOwner&&native?'foreign':'owner'}},items:[{qualifiedName:namespace+'.'+name,definition,definitionHash,catalogHash:source.catalogHash}],nextCursor:null};
  }
  if(method==='tools.call'){
   const name=p.name??p.qualifiedName?.split('.').slice(1).join('.'),args=p.arguments;
   if(name==='status')return{...structuredClone(status),epoch:state.epoch};
   if(name==='projects'){
    if(c.project?.badProjects)return{projects:[]};
    const project={nativeId:c.project?.missing?'foreign':definition.project.nativeId,source:'native',name:'Synthetic project',paths:c.project?.roots??['/']};
    return{source:'native',observedAt:new Date(now).toISOString(),projects:c.project?.duplicate?[project,project]:[project]};
   }
   if(name==='operation'){
    if(state.lookup==='generic')throw new IvyError('not_found','Generic fixture absence.');
    if(state.lookup==='absent')throw new IvyError('not_found','Exact fixture owner absence.','not_executed',{kind:'agent_operation_absent',operationId:args.operationId,serviceNodeId:state.absenceOwner??'owner',epoch:state.epoch});
    return ownerReceipt(state.lookup);
   }
   if(name==='prevent'){
    if(state.preventError)throw new IvyError('fixture_owner_unavailable','Prevention reply is unavailable.','unknown');
    if(state.lookup==='absent')state.lookup='failed';
    return ownerReceipt(state.lookup);
   }
   if(name==='invoke'){
    if(state.staleEnvelope&&envelopeRevision===1){envelopeRevision=2;throw new IvyError('tool_definition_changed','Original stale envelope.','not_executed');}
    state.lookup=state.afterInvoke??state.invoke;
    if(state.loseInvoke)throw new IvyError('outcome_unknown','Original invoke reply lost.','unknown');
    return ownerReceipt(state.invoke);
   }
  }
  throw Error('Unexpected synthetic RPC '+method+' '+JSON.stringify(p));
 };
 const client={async request(method,params){const event={kind:'rpc',method,params:structuredClone(params)};events.push(event);
  try{const value=await rpc(method,params);event.result={value:structuredClone(value)};return value;}catch(e){const err=IvyError.from(e);event.result={error:{...error(err),...(err.details===undefined?{}:{details:err.details})}};throw e;}}};
 const open=()=>{store=new ChatStore(client,null,root);driver=new ChatNativeDriver(store,new ChatAdmission(definition),{serviceNodeId:'chat-provider',generation:7});};open();
 body=await bodies(store,definition,input,c.spec);c.body=structuredClone(body);c.definition=definition;c.origin=structuredClone(origin);
 const parent=()=>c.parent==='missing'?'missing':'original-input';
 const guard=async()=>{guardCount++;const event={kind:'guard'};events.push(event);
  if(state.disconnectAtGuard)node.connected=false;
  if(state.guardError||state.secondGuardError&&guardCount===2){const e=new IvyError('fixture_claim_lost','Original Main/queue claim is no longer current.');event.result={error:error(e)};throw e;}
  event.result={value:null};};
 const document=call=>({pin:call.pin,value:call.value});
 const selected=()=>{const p=pins.get('call');if(!p)throw new IvyError('fixture_pin_missing','Original native call pin is unavailable.');return p;};
 const snapshot=output=>{
  const db=new DatabaseSync(file),docs=db.prepare("SELECT d.id,d.name,d.parent_id,d.current_revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key IN ('chat-bridge/evidence','chat-bridge/native-call') ORDER BY d.name").all(),blobs=db.prepare('SELECT id,key,name,parent_id,bytes FROM blobs ORDER BY name,parent_id').all();
  for(const r of [...docs,...blobs])aliases.set(r.id,'<local:'+r.name+'>');
  const writes=db.prepare('SELECT rowid,value_json FROM native_evidence_test_writes WHERE rowid>? ORDER BY rowid').all(lastWrite);lastWrite=writes.at(-1)?.rowid??lastWrite;db.close();
  const normalize=v=>typeof v==='string'?(aliases.get(v)??v):Array.isArray(v)?v.map(normalize):v&&typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,v])=>[k,normalize(v)])):v;
  const expected=normalize({output,documents:docs.map(r=>({id:r.id,name:r.name,parentId:r.parent_id,revision:r.current_revision,value:JSON.parse(r.value_json)})),blobs:blobs.map(r=>({id:r.id,key:r.key,name:r.name,parentId:r.parent_id,byteLength:r.bytes.length,contentHash:digest(r.bytes)})),writes:writes.map(r=>JSON.parse(r.value_json))});
  const transport=structuredClone(events);events.length=0;return{...expected,transport};
 };
 c.expected=[];events.length=0;
 try{for(const s of c.steps){const output=await result(async()=>{switch(s.action){
  case'prepare':{const call=await driver.prepare(parent(),'driver-original-call',body.request,origin,s.projection?{excludeTurns:true}:undefined);pins.set('call',call.pin);return document(call);}
  case'get':return document(await driver.get(parent(),s.current?selected().objectId:selected()));
  case'advance':return document(await driver.advance(parent(),selected(),guard));
  case'prevent':return document(await driver.prevent(parent(),selected(),guard));
  case'verify-project':return driver.verifyProject();
  case'reopen':store.close();open();return;
  case'state':Object.assign(state,structuredClone(s.value));return;
  case'patch':Object.assign(s.target==='node'?node:s.target==='origin'?origin:s.target==='requestParams'?body.request.params:body[s.target],structuredClone(s.value));return;
  case'rewrite':{const old=await store.read('chat-bridge/native-call',selected().objectId,parent());await store.write('chat-bridge/native-call',{...old.value,...s.value},mutation(old.pin.objectId,'fixture-rewrite'),{objectId:old.pin.objectId,expectedRevision:old.pin.revision});return;}
  case'rename':{const db=new DatabaseSync(file);db.prepare('UPDATE documents SET name=? WHERE id=?').run(s.name,selected().objectId);db.close();return;}
  case'corrupt-receipt':{const old=await store.read('chat-bridge/native-call',selected().objectId,parent()),pin=old.value.evidence,db=new DatabaseSync(file);const record=db.prepare('SELECT value_json FROM revisions WHERE id=? AND revision=?').get(pin.objectId,pin.revision);const v=JSON.parse(record.value_json);v.contentHash=digest('tampered');db.prepare('UPDATE revisions SET value_json=? WHERE id=? AND revision=?').run(JSON.stringify(v),pin.objectId,pin.revision);db.close();return;}
  default:throw Error('Unknown action '+s.action);
 }});c.expected.push(snapshot(output));}}
 finally{store.close();rmSync(root,{recursive:true,force:true});}
}
for(const c of cases)await execute(c);
for(const c of cases.filter(c=>/^(prepare-recover|dispatch-retain|prepare-thread|prepare-turn|resume-|observed-progress|unchanged-observation|absent-resume|unscoped-|lost-invoke-reply|owner-outcome|prevention-retains|prevention-race|stale-envelope-rechecks|project-(posix|windows|unc|extended|drive-root|null-resume)$)/.test(c.label))) {
 if(c.expected.some(s=>s.output.error))throw Error('Expected successful original workflow failed '+c.label+': '+JSON.stringify(c.expected.map(s=>s.output)));
}
const paths=['/','//','/a/../','/a/../../b/','/A/\\','\\','D:relative','D:/','D:\\Testing\\Project\\..\\Project\\','\\\\server\\share\\','\\\\?\\D:\\Project\\..\\other','/lone\ud800/../\udfff','D:/İ/Σ/\ud800','relative','','///a//b/.','/a/..\\'];
let seed=0x91c0ffee;const rnd=()=>{seed=(Math.imul(seed,1664525)+1013904223)>>>0;return seed;};
const parts=['a','B','.','..','İ','Σ','\ud800','\udfff','😀',''];
for(let i=0;i<512;i++){const windows=i%2===0,prefix=windows?['D:\\','e:/','\\\\Server\\Share\\','\\\\?\\D:\\'][i%4]:'/';paths.push(prefix+Array.from({length:1+rnd()%8},()=>parts[rnd()%parts.length]).join(windows?'\\':'/')+(i%3===0?'/':''));}
process.stdout.write(JSON.stringify({now,instructions,seeds,cases,paths:await Promise.all(paths.map(async value=>({value,result:await result(()=>nativePath(value))})))}));
