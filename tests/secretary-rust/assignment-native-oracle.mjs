import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {load,settings,owner,rootId,at} from './inbox-fixtures.mjs';
const {AssignmentRunner}=await load('services/secretary/src/assignment-runner.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {IvyError,hashJson,canonical}=await load('packages/sdk/src/node.js');
const catalog=JSON.parse(readFileSync('specs/native/codex-0.159.2/catalog.json','utf8'));
const digest=c=>'sha256:'+c.repeat(64),key='secretary/assignment-native-call';
const target={serviceNodeId:'fixture.agent',hostId:'fixture-host',nativeVersion:catalog.version,nativeExecutableHash:catalog.nativeExecutableHash,catalogHash:hashJson(catalog)};
const rows=[];
const phaseState=phase=>({phase,...(phase==='accepted'?{epoch:null,requestId:null,reply:null}:phase==='failed'?{reply:{error:{code:1,message:'failed'}},code:'native_failed'}:phase==='outcome_unknown'?{reply:null,code:'native_unknown'}:phase==='succeeded'?{}:{reply:null})});
async function add(label,c={}){
 const trace=[],steps=[];let owners=0,calls=0;
 const method=c.method??'turn/start',operationId='original-operation',params=c.params??{threadId:'original-thread',input:[{type:'text',text:'regenerated prompt'}]};
 const retained=c.retained===false?null:{operationId,method,params:{threadId:'original-thread',input:[{type:'text',text:'retained prompt\ud800'}]},definitionHash:digest('b'),...c.retained};
 const effective=retained??{operationId,method,params:method==='thread/start'&&params.projectId===''?{...params,projectId:null}:params,definitionHash:digest('b')};
 const operation={schemaVersion:1,operationId,callerPrincipalId:settings.identity.principalId,serviceNodeId:target.serviceNodeId,nativeVersion:target.nativeVersion,nativeExecutableHash:target.nativeExecutableHash,method,params:effective.params,requestHash:hashJson({method,params:effective.params}),createdAt:at,updatedAt:at,phase:'succeeded',epoch:'epoch',requestId:42,reply:{result:null},code:null,...c.operation};
 const absence={kind:'agent_operation_absent',operationId,serviceNodeId:target.serviceNodeId,epoch:'epoch'};
 const controller=new AbortController(),cancel=new IvyError('fixture_cancelled','Fixture cancelled.');if(c.cancelBefore)controller.abort(cancel);
 const client={async request(method,p,options){
  trace.push({method,params:structuredClone(p),timeout:options?.timeoutMs??35000});
  let value,error;
  if(method==='serviceNodes.get'){owners++;value={...owner,...(c.ownerAt===owners?c.owner:{})};}
  else if(method==='tools.list')value={provider:{node:{serviceNodeId:target.serviceNodeId}},items:[{qualifiedName:p.namespace+'.'+p.namePrefix,definition:{namespace:p.namespace,name:p.namePrefix,interfaceVersion:p.namespace==='agent'?'1.0.0':target.nativeVersion,inputSchema:{},outputSchema:{}},definitionHash:digest(p.namespace==='agent'?'a':c.nativeHash??'b'),catalogHash:target.catalogHash}],nextCursor:null};
  else if(method==='tools.call'){
   calls++;
   if(p.qualifiedName==='agent.operation'){
    if(c.absent&&calls===1)error={code:'not_found',message:'Absent.',outcome:'not_executed',...(c.noAbsenceProof?{}:{details:absence})};
    else value=c.afterPhase?{...operation,...phaseState(c.afterPhase)}:operation;
   }else if(p.qualifiedName==='agent.invoke'){
    assert.deepEqual(p.arguments.params,effective.params,'Original bytes required');assert.equal(p.operationId,operationId);
    if(c.invokeError)error={message:'Fixture send failure.',...c.invokeError};else value=c.invokedPhase?{...operation,...phaseState(c.invokedPhase)}:operation;
   }else assert.fail('Unexpected tool '+p.qualifiedName);
  }else assert.fail('Unexpected RPC '+method);
  if(c.cancelAfter===trace.length)controller.abort(cancel);
  if(error){steps.push({method,error});throw new IvyError(error.code,error.message,error.outcome,error.details);}
  steps.push({method,value:structuredClone(value)});return value;
 }};
 const store=new SecretaryStore(client,rootId);
 if(retained){const value=canonical(retained);store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run('retained',key,operationId,1,value);store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run('retained',1,value);}
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings,owner:{serviceNodeId:'fixture.secretary'},signal:controller.signal});
 const runner=new AssignmentRunner(engine),execution={nativeTarget:c.noTarget?null:target};
 let result;try{result={value:await runner.completed(runner.owner(execution),operationId,method,params)};}catch(cause){const e=IvyError.from(cause);result={code:e.code,message:e.message,outcome:e.outcome};}
 const saved=store.technicalNamed(key,operationId);store.close();
 rows.push({input:{label,execution,operationId,method,params,retained,steps,cancelBefore:c.cancelBefore??false,cancelAfter:c.cancelAfter??null},expected:{result,trace,saved:saved?{name:saved.name,revision:saved.pin.revision,value:saved.value}:null}});
}
for(const retained of [false,{}])for(const phase of ['succeeded','accepted','dispatched','failed','outcome_unknown'])await add('phase-'+phase+'-'+Boolean(retained),{retained,operation:phaseState(phase)});
await add('fresh-thread-project-optout',{retained:false,method:'thread/start',params:{cwd:'/workspace',projectId:''}});
await add('fresh-thread-explicit-project',{retained:false,method:'thread/start',params:{cwd:'/workspace',projectId:'original-project'}});
await add('retained-thread-start',{method:'thread/start'});
for(const retained of [false,{}]){
 await add('absent-dispatch-'+Boolean(retained),{retained,absent:true});
 await add('absent-no-proof-'+Boolean(retained),{retained,absent:true,noAbsenceProof:true});
 for(const outcome of ['unknown','completed','not_executed'])await add('send-'+outcome+'-'+Boolean(retained),{retained,absent:true,invokeError:{code:'fixture_failure',outcome}});
 await add('cancel-before-'+Boolean(retained),{retained,cancelBefore:true});
}
for(const phase of ['accepted','failed'])await add('invoked-'+phase,{absent:true,invokedPhase:phase});
await add('uncertain-send-pending-reconciliation',{absent:true,invokeError:{code:'fixture_failure',outcome:'unknown'},afterPhase:'accepted'});
for(const field of ['operationId','method'])await add('retained-mismatch-'+field,{retained:{[field]:'different'}});
for(const field of ['callerPrincipalId','serviceNodeId','nativeExecutableHash','requestHash'])await add('evidence-mismatch-'+field,{operation:{[field]:field.endsWith('Hash')?digest('e'):'other'}});
await add('native-definition-changed',{absent:true,nativeHash:'e'});
await add('missing-target',{noTarget:true});
for(const fresh of [true,false])await add('owner-changed-'+fresh,{retained:fresh?false:{},absent:true,ownerAt:1,owner:{principalId:'other'}});
await add('owner-changed-after-admission',{retained:false,absent:true,ownerAt:2,owner:{connected:false}});
for(const cancelAfter of [1,2,3,4])await add('cancel-after-'+cancelAfter,{retained:false,absent:true,cancelAfter});
process.stdout.write(JSON.stringify({settings,rootId,rows}));
