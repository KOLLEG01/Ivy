import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
}});
const evidence=await import('../../packages/sdk/src/native-evidence.ts');
const {validateAgent,validateAgentFrame}=await import('../../packages/contracts/src/agent-validation.ts');
const {hashJson,digest}=await import('../../packages/contracts/src/canonical.ts');
const {reconcileNativeOperation}=await import('../../packages/sdk/src/native-operation.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const target={serviceNodeId:'native-agent',hostId:'fixture',nativeVersion:'0.142.3',nativeExecutableHash:digest('executable'),catalogHash:digest('catalog')};
const expected={...target,callerPrincipalId:'caller',operationId:'original',method:'thread/read',params:{threadId:'thread'},epoch:'epoch'};
const time='2026-01-02T03:04:05.006Z';
const base={schemaVersion:1,operationId:'original',callerPrincipalId:'caller',serviceNodeId:target.serviceNodeId,nativeVersion:target.nativeVersion,
  nativeExecutableHash:target.nativeExecutableHash,method:expected.method,params:expected.params,requestHash:hashJson({method:expected.method,params:expected.params}),
  createdAt:time,updatedAt:time,phase:'accepted',epoch:null,requestId:null,reply:null,code:null};
function operation(phase){return {...base,phase,...(phase==='accepted'?{}:{epoch:'epoch',requestId:42}),
  ...(phase==='succeeded'?{reply:{result:{ok:true}}}:phase==='failed'?{code:'native_error',reply:{error:{code:-1,message:'failed'}}}:phase==='outcome_unknown'?{code:'outcome_unknown'}:{})};}
const status={...target,epoch:'epoch',pid:42,state:'ready',observedAt:time,code:null,initialized:{},pendingInputs:0,
  operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
const read={schemaVersion:1,observationId:'read',callerPrincipalId:'caller',serviceNodeId:target.serviceNodeId,nativeVersion:target.nativeVersion,
  nativeExecutableHash:target.nativeExecutableHash,catalogHash:target.catalogHash,epoch:'epoch',requestId:42,observedAt:time,requestHash:base.requestHash,
  method:expected.method,params:expected.params,reply:{result:{thread:{id:'thread'}}}};
const error=e=>({code:e.code??null,message:e.message,outcome:e.outcome??null});
const rows=[];
function add(kind,value,other=expected,code){
  let result;
  try{
    let returned;
    if(kind==='instant')returned=evidence.nativeInstant(value,code);
    else if(kind==='status')evidence.validateNativeStatus(value,other,code);
    else if(kind==='operation')evidence.validateNativeOperation(value,other,code);
    else if(kind==='read')evidence.validateNativeRead(value,other,code);
    else if(kind==='progress')evidence.validateNativeProgress(value,other,code);
    else if(kind==='frame')validateAgentFrame(value,other);
    else validateAgent(other,value);
    result={ok:true,value:returned??null};
  }catch(e){result={ok:false,error:error(e)};}
  rows.push({input:JSON.stringify({kind,value,other,code}),result:JSON.stringify(result)});
}
for(const at of [-8640000000000000,-8639999999999999,-62167219200000,-62198755200000,-1,0,1,253402300799999,253402300800000,8639999999999999,8640000000000000])add('instant',new Date(at).toISOString());
let seed=0x13579bdf;
for(let i=0;i<2048;i++){
  seed=(Math.imul(seed,1664525)+1013904223)>>>0;
  const at=Math.trunc((seed/0xffffffff*2-1)*8640000000000000), text=new Date(at).toISOString();
  add('instant',text);add('instant',text.replace('Z','+00:00'));
}
for(const s of ['', 'not a date', '2026-01-02', '2026-01-02T03:04:05Z','2026-01-02T03:04:05.0060Z','2026-01-02t03:04:05.006z',
  '2026-02-29T00:00:00.000Z','2000-02-29T00:00:00.000Z','1900-02-29T00:00:00.000Z','0000-02-29T00:00:00.000Z',
  '+000000-01-01T00:00:00.000Z','-000000-01-01T00:00:00.000Z','+009999-01-01T00:00:00.000Z',
  '+275760-09-13T00:00:00.001Z','-271821-04-19T23:59:59.999Z','2026-01-01T24:00:00.000Z','2026-01-01T23:59:60.000Z','\ud800'])add('instant',s,expected,'custom_instant');
for(const [kind,valid] of [['status',status],['operation',operation('succeeded')],['read',read]]){
  add(kind,valid);
  for(const key of Object.keys(valid)){
    const missing={...valid};delete missing[key];add(kind,missing);
    for(const value of [null,true,0,-1,9007199254740992,'','wrong','\ud800',[],{},time])add(kind,{...valid,[key]:value});
  }
  add(kind,{...valid,extra:true});
  for(const key of Object.keys(expected))add(kind,valid,{...expected,[key]:key==='params'?{threadId:'different'}:'different'},'custom_evidence');
}
const noHost={...target};delete noHost.hostId;add('status',status,noHost);
for(const phase of ['accepted','dispatched','outcome_unknown','succeeded','failed']){
  const value=operation(phase);add('operation',value);add('progress',value,value);
  for(const next of ['accepted','dispatched','outcome_unknown','succeeded','failed'])add('progress',value,operation(next));
  for(const key of Object.keys(value))for(const change of [null,'different'])add('progress',value,{...value,[key]:change},'custom_progress');
  add('progress',value,{...value,updatedAt:'2026-01-02T03:04:05.007Z'});
  add('progress',value,{...value,updatedAt:'2026-01-02T03:04:05.005Z'});
}
add('operation',{...base,params:{a:1,b:'\ud800'},requestHash:hashJson({method:base.method,params:{b:'\ud800',a:1}})},
  {...expected,params:{b:'\ud800',a:1}});
add('agent',{},'MissingDefinition');
for(const value of [null,true,false,0,-1,1.5,9007199254740991,-9007199254740991,9007199254740992,'','id','\ud800','x'.repeat(256),'x'.repeat(257),[],{},
  {result:null},{result:[]},{result:{},id:'id',extra:true},{error:null},{error:[]},{error:{}},{error:{code:0,message:''}},
  {error:{code:-9007199254740991,message:'\ud800',data:{},extra:true},id:7},{error:{code:0.5,message:'wrong'}},{result:{},error:{code:1,message:'both'}}]){
  add('frame',value,'request-id');add('frame',value,'reply');
}
const call={operationId:'original',method:expected.method,params:expected.params,definitionHash:digest('definition')};
const absence={kind:'agent_operation_absent',operationId:'original',serviceNodeId:target.serviceNodeId,epoch:'epoch'};
const scenarios=[
  {name:'absence without authority',reads:[absence]},
  {name:'submitted result',reads:[absence],submit:operation('succeeded')},
  {name:'lost dispatch reply',reads:[absence,operation('succeeded')],submit:null},
  {name:'still absent',reads:[absence,absence],submit:null},
  {name:'observed marker',reads:[absence],wasObserved:true,submit:null},
  {name:'previous disappeared',previous:operation('dispatched'),reads:[absence],submit:null,conflict:'custom_absence'},
  {name:'terminal offline',previous:operation('succeeded'),reads:[]},
  {name:'failed terminal offline',previous:operation('failed'),reads:[]},
  {name:'invalid prior identity',previous:{...operation('succeeded'),operationId:'wrong'},reads:[]},
  {name:'regression',previous:operation('outcome_unknown'),reads:[operation('dispatched')]},
  {name:'late result',previous:operation('outcome_unknown'),reads:[operation('succeeded')]},
  {name:'same receipt',previous:operation('dispatched'),reads:[operation('dispatched')]},
  {name:'same timestamp and phase',previous:operation('dispatched'),reads:[{...operation('dispatched'),code:'changed'}]},
  {name:'save failed',reads:[operation('succeeded')],retainError:true},
  {name:'guard failed',reads:[absence],submitError:true},
  {name:'generic not found',reads:[{throw:'not_found'}],submit:null},
  {name:'unknown lookup',reads:[{throw:'outcome_unknown'}]},
  {name:'wrong observed identity',reads:[{...operation('dispatched'),callerPrincipalId:'other'}]},
  {name:'wrong submitted identity',reads:[absence],submit:{...operation('succeeded'),operationId:'other'}},
  {name:'submitted absence',reads:[absence],submit:absence},
];
for(const scenario of scenarios){
  const trace=[], reads=structuredClone(scenario.reads);
  const owner={checkOperation(observed,original){trace.push('check');evidence.validateNativeOperation(observed,{...expected,...original});},
    async operation(original){trace.push({read:original});const value=reads.shift();if(!value)throw Error('unexpected read');if(value.throw)throw new IvyError(value.throw,'lookup failed',value.throw==='outcome_unknown'?'unknown':'not_executed');return value;}};
  const options={owner,call,journal:{current:7,previous:scenario.previous??null,wasObserved:scenario.wasObserved??false,
    async retain(observed){trace.push({retain:observed});if(scenario.retainError)throw new IvyError('outcome_unknown','save failed','unknown');return 8;}},absenceConflictCode:scenario.conflict};
  if('submit'in scenario||scenario.submitError)options.onAbsent=async(original,absent)=>{
    trace.push({submit:original,absence:absent});if(scenario.submitError)throw new IvyError('claim_changed','guard failed');return structuredClone(scenario.submit??undefined);
  };
  let result;try{result={ok:true,value:await reconcileNativeOperation(options)};}catch(e){result={ok:false,error:error(e)};}
  rows.push({input:JSON.stringify({kind:'reconcile',scenario,call,expected}),result:JSON.stringify({trace,result})});
}
process.stdout.write(JSON.stringify(rows));
