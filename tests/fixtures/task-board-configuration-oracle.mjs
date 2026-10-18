import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const u=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(u)))return next(u.href,context);}
  return next(specifier,context);
}});
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {scalarTypes}=await import('../../packages/contracts/src/schema.ts');
const {validateTaskBoard}=await import('../../packages/contracts/src/task-board-validation.ts');
const {newOperationId,BoundToolClient}=await import('../../packages/sdk/src/client.ts');
const recoverySource=readFileSync('services/task-board/src/runtime/native-recovery.ts','utf8');
const recoveryMethods=recoverySource.slice(recoverySource.indexOf('export const missingNativeThread'),recoverySource.indexOf('async function unstartedAttempt')).replaceAll('export ','');
const {freshContextRecovery}=new Function('requireThat',stripTypeScriptTypes(recoveryMethods)+'\nreturn {freshContextRecovery};')(requireThat);
const source=readFileSync('services/task-board/src/runtime/configuration.ts','utf8');
const body=source.slice(source.indexOf('export const configurationName')).replaceAll('export ','');
const functions=new Function('requireThat','scalarTypes','validateTaskBoard','newOperationId','BoundToolClient','freshContextRecovery',stripTypeScriptTypes(body)+'\nreturn {initialConfiguration,configurationView,executionWithDefaults,effectiveExecution,checkConfiguration,supportsExecutionOptions};')
  (requireThat,scalarTypes,(...args)=>{
    activeTrace.push(['validate',...structuredClone(args)]);
    // Production Rust admission remains an external mandatory binding. The
    // original additionally validates each captured Configuration with its AJV schema.
    requireThat(!activeInput.reject&&args[0]==='Configuration'&&hashJson(args[1])===hashJson(activeInput.value),'fixture_admission','Fixture configuration was not admitted.');
    validateTaskBoard(...args);
  },newOperationId,BoundToolClient,freshContextRecovery);
const support=readFileSync('tests/fixtures/task-board-operations-oracle.mjs','utf8');
const Memory=new Function('hashJson','IvyError',support.slice(support.indexOf('class Memory {'),support.indexOf('const begin='))+'\nreturn Memory;')(hashJson,IvyError);
const pin=(objectId,revision=1)=>({objectId,revision});
const options={model:'fixture-model',reasoningEffort:'high',serviceTier:'standard'};
const blank={model:null,reasoningEffort:null,serviceTier:'standard'};
const node={serviceNodeId:'native-agent',hostId:'fixture-host',nativeVersion:'0.159.2',connected:true,synced:true,ready:true};
const model={id:'fixture-id',model:'fixture-model',isDefault:true,supportedReasoningEfforts:[{reasoningEffort:'high'}]};
const stringTier={type:'object',properties:{serviceTier:{anyOf:[{type:'string'},{type:'null'}]}}};
const config=(nativeOptions=options,executionRequirement=null)=>({schemaVersion:1,defaults:{...functions.initialConfiguration().defaults,executionRequirement,nativeOptions}});
const run={taskId:'task',phase:'failed',turnId:null,cancellation:null,target:{hostId:'old-host'},plan:{turnStart:{model:'old-model',effort:'low',serviceTier:'fast'}}};
const task={fields:{},primaryResourceRef:null,lastRun:null,lastHistory:null};
const record=(id,value,key='task-board/run',name='record',parentId='root')=>({id,value,key,name,parentId});
const savedConfig=record('config',config(),'task-board/configuration','TaskBoard configuration');
const recovered=record('h1',{taskId:'task',priorRun:pin('run'),kind:'recovered',actor:{source:'scheduler'},previousHistory:null},'task-board/history');
const rows=[];
const add=(name,method,input={})=>rows.push({name,method,input});
add('initial preferences','initial');
add('configuration read does not create','view');
add('saved configuration pin and value','view',{records:[savedConfig]});
add('configuration root is scoped','view',{records:[{...savedConfig,parentId:'other'}]});
add('configuration name is exact','view',{records:[{...savedConfig,name:'other'}]});
const defaults=config().defaults;
for(const [name,fields,inherited] of [
  ['empty fields',{},defaults],['null options',{nativeOptions:null},defaults],['null fields inherit',{executionRequirement:null,nativeOptions:{model:null,reasoningEffort:null,serviceTier:null}},defaults],
  ['explicit automatic',{executionRequirement:{kind:'automatic'}},defaults],['explicit host',{executionRequirement:{kind:'host',hostId:'other'}},defaults],
  ['explicit standard clears inherited fast',{nativeOptions:{serviceTier:'standard'}},{...defaults,nativeOptions:{...options,serviceTier:'fast'}}],
  ['partial effort override',{nativeOptions:{reasoningEffort:'low'}},defaults],['empty strings remain explicit',{nativeOptions:{model:'',reasoningEffort:'',serviceTier:''}},defaults],
  ['missing inherited tier defaults to standard',{}, {...defaults,nativeOptions:{model:null,reasoningEffort:null}}],
  ['null inherited tier defaults to standard',{}, {...defaults,nativeOptions:{...blank,serviceTier:null}}],
  ['UTF16 fields and defaults',{nativeOptions:{model:'new\ud800'}},{...defaults,executionRequirement:{kind:'host',hostId:'host\udfff'}}],
])add('merge: '+name,'merge',{fields,defaults:inherited});
const effective=(name,patch={},records=[])=>add('effective: '+name,'effective',{task:{...task,...patch},records});
effective('initial defaults');effective('current saved defaults',{},[savedConfig]);
const primary={serviceNodeId:'native-agent',namespace:'codex',kind:'thread',nativeId:'thread'};
const continuing={lastRun:pin('run'),primaryResourceRef:primary};
effective('saved allocation survives configuration change',continuing,[record('run',run),savedConfig]);
effective('explicit standard and automatic override',{...continuing,fields:{executionRequirement:{kind:'automatic'},nativeOptions:{model:null,reasoningEffort:'high',serviceTier:'standard'}}},[record('run',run),savedConfig]);
effective('native nonstring values are not selected',continuing,[record('run',{...run,plan:{turnStart:{model:17,effort:false,serviceTier:'other'}}})]);
effective('missing planned options',continuing,[record('run',{...run,plan:{turnStart:{}}})]);
for(const serviceTier of ['standard','flex',null,'other'])effective('retained tier '+serviceTier,continuing,[record('run',{...run,plan:{turnStart:{...run.plan.turnStart,serviceTier}}})]);
const fresh={lastRun:pin('run'),lastHistory:pin('h1')};
effective('scheduler recovery retains empty allocation',fresh,[record('run',run),recovered,savedConfig]);
effective('user recovery uses current defaults',fresh,[record('run',run),{...recovered,value:{...recovered.value,actor:{source:'user'}}},savedConfig]);
effective('no recovery history uses current defaults',{lastRun:pin('run')},[record('run',run),savedConfig]);
for(const [name,patch] of [['running',{phase:'running'}],['has turn',{turnId:'turn'}],['cancelled',{cancellation:{}}]])effective(name+' uses current defaults',fresh,[record('run',{...run,...patch}),recovered,savedConfig]);
effective('foreign recovery history refuses',fresh,[record('run',run),{...recovered,value:{...recovered.value,taskId:'other'}}]);
effective('history cycle refuses',fresh,[record('run',run),{...recovered,value:{...recovered.value,kind:'updated',previousHistory:pin('h1')}}]);
effective('unavailable retained run refuses',continuing);
effective('primary without run does not read native',{primaryResourceRef:primary},[savedConfig]);
effective('UTF16 retained options',continuing,[record('run',{...run,target:{hostId:'host\ud800'},plan:{turnStart:{model:'model\udfff',effort:'effort\ud800',serviceTier:'flex'}}})]);
const supports=(name,input={})=>add('supports: '+name,'supports',{node,options,models:{'native-agent':[{data:[model],nextCursor:null}]},...input});
supports('model and effort');supports('native defaults need no model catalog',{options:blank});
supports('model id alias',{options:{...options,model:'fixture-id'}});
supports('unknown model',{options:{...options,model:'missing-model'}});
supports('unsupported reasoning',{options:{...options,reasoningEffort:'max'}});
supports('reasoning chooses default model',{options:{...options,model:null}});
supports('missing default model',{options:{...options,model:null},models:{'native-agent':[{data:[{...model,isDefault:false}],nextCursor:null}]}});
supports('first matching alias is authoritative',{models:{'native-agent':[{data:[{...model,supportedReasoningEfforts:[]},model],nextCursor:null}]}});
supports('later model page',{models:{'native-agent':[{data:[],nextCursor:'next'},{data:[model],nextCursor:null}]}});
supports('empty final cursor',{models:{'native-agent':[{data:[model],nextCursor:''}]}});
supports('repeated model cursor refuses',{models:{'native-agent':[{data:[model],nextCursor:'same'},{data:[],nextCursor:'same'}]}});
supports('longer model cursor cycle refuses',{models:{'native-agent':[{data:[],nextCursor:'a'},{data:[],nextCursor:'b'},{data:[model],nextCursor:'a'}]}});
for(const count of [1000,1001])supports('complete snapshot '+count,{models:{'native-agent':[{data:Array.from({length:count},()=>model),nextCursor:null}]}});
for(const count of [400,401])supports('paged snapshot '+(600+count),{models:{'native-agent':[{data:Array.from({length:600},()=>model),nextCursor:'next'},{data:Array.from({length:count},()=>model),nextCursor:null}]}});
for(const nativeVersion of [null,'','invalid','bad\ud800'])supports('native version '+nativeVersion,{node:{...node,nativeVersion}});
supports('epoch failure before tool discovery',{rpcFailure:{method:'system.status',code:'fixture_epoch',outcome:'not_executed'}});
supports('original failed model call propagates',{rpcFailure:{method:'tools.call',code:'outcome_unknown',outcome:'unknown'}});
supports('all pages read before selecting',{models:{'native-agent':[{data:[model],nextCursor:'next'},{error:{code:'fixture_page',outcome:'not_executed'}}]}});
supports('wrong selected provider',{provider:'other'});
supports('wrong native interface',{interfaceVersion:'0.1.0'});
supports('missing tool catalog binding',{missingBinding:'model/list'});
supports('exact tool on a later catalog page',{pagedBinding:true});
supports('changed read definition refresh preserves operation',{refresh:true});
for(const serviceTier of ['fast','flex'])supports(serviceTier+' checks both start schemas',{options:{...blank,serviceTier}});
supports('nullable string tier supported',{options:{...blank,serviceTier:'fast'},tierSchemas:{'thread/start':stringTier,'turn/start':stringTier}});
supports('missing thread tier refuses',{options:{...blank,serviceTier:'fast'},tierSchemas:{'thread/start':{type:'object'},'turn/start':stringTier}});
supports('missing turn tier refuses',{options:{...blank,serviceTier:'fast'},tierSchemas:{'thread/start':stringTier,'turn/start':{type:'object'}}});
supports('referenced tier schema supported',{options:{...blank,serviceTier:'fast'},tierSchemas:{'thread/start':{$defs:{Start:stringTier},$ref:'#/$defs/Start'},'turn/start':stringTier}});
supports('both tier lookups begin after first error',{options:{...blank,serviceTier:'fast'},rpcFailure:{method:'tools.list',name:'thread/start',code:'fixture_binding',outcome:'not_executed'}});
supports('binding second failure propagates',{options:{...blank,serviceTier:'fast'},rpcFailure:{method:'tools.list',name:'turn/start',code:'fixture_binding',outcome:'unknown'}});
supports('slow thread failure observes turn error first',{options:{...blank,serviceTier:'fast'},delayBinding:'thread/start',bindingFailures:{'thread/start':'fixture_slow','turn/start':'fixture_fast'}});
const check=(name,input={})=>add('check: '+name,'check',{value:config(),nodePages:[{items:[node],nextCursor:null}],models:{'native-agent':[{data:[model],nextCursor:null}]},...input});
check('ready registered selection');
check('validation failure precedes discovery',{reject:true});
check('registered host later page',{value:config(options,{kind:'host',hostId:'fixture-host'}),nodePages:[{items:[],nextCursor:'more'},{items:[node],nextCursor:null}]});
check('unregistered host',{value:config(options,{kind:'host',hostId:'missing-host'})});
check('unregistered host still refuses allowUnavailable',{value:config(options,{kind:'host',hostId:'missing-host'}),allowUnavailable:true});
check('no ready host rejects concrete options',{nodePages:[{items:[],nextCursor:null}]});
check('no ready host can retain selection',{nodePages:[{items:[],nextCursor:null}],allowUnavailable:true});
check('no concrete options permits offline host',{value:config(blank),nodePages:[{items:[{...node,ready:false}],nextCursor:null}]});
check('nonstandard offline tier is unverified',{value:config({...blank,serviceTier:'fast'}),nodePages:[{items:[{...node,ready:false}],nextCursor:null}]});
for(const status of ['connected','synced','ready'])check(status+' is required',{nodePages:[{items:[{...node,[status]:false}],nextCursor:null}]});
check('registered offline host is selectable only when allowed',{value:config(options,{kind:'host',hostId:'fixture-host'}),nodePages:[{items:[{...node,ready:false}],nextCursor:null}],allowUnavailable:true});
check('existing native owner cannot borrow another catalog',{serviceNodeId:'saved-owner'});
check('empty owner selector is not a filter',{serviceNodeId:''});
check('registered host filters otherwise ready nodes',{value:config(options,{kind:'host',hostId:'offline-host'}),nodePages:[{items:[node,{...node,serviceNodeId:'offline-agent',hostId:'offline-host',ready:false}],nextCursor:null}]});
check('model discovery across eligible hosts',{nodePages:[{items:[{...node,serviceNodeId:'first-agent'},node],nextCursor:null}],models:{'first-agent':[{data:[],nextCursor:null}],'native-agent':[{data:[model],nextCursor:null}]}});
check('unsupported model detail retains requested fields',{value:config({...options,model:'missing-model'},{kind:'host',hostId:'fixture-host'})});
check('first eligible host error is not skipped',{nodePages:[{items:[{...node,serviceNodeId:'first-agent',nativeVersion:null},node],nextCursor:null}]});
check('node page failure propagates',{rpcFailure:{method:'serviceNodes.list',code:'fixture_nodes',outcome:'unknown'}});
check('UTF16 selected values survive diagnostic',{value:config({...options,model:'missing\ud800',reasoningEffort:'effort\udfff'},{kind:'host',hostId:'host\ud800'}),nodePages:[{items:[{...node,hostId:'host\ud800'}],nextCursor:null}]});
let activeTrace,activeInput;
for(const row of rows){
  const i=row.input,store=new Memory();activeTrace=store.trace;activeInput=i;
  for(const r of i.records??[])store.docs.set(r.id,{key:r.key,name:r.name,pin:pin(r.id),value:r.value,metadata:{parentId:r.parentId},revision:{contentHash:hashJson(r.value)}});
  let nodePage=0,refreshed=false;
  const modelPages=new Map(),issued=new Map();
  const client={async request(method,params){
    const p=structuredClone(params);
    if(method==='tools.call'){
      requireThat(/^11111111-1111-4111-8111-111111111111:\d+:[0-9a-f-]{36}$/.test(p.operationId),'fixture_identity','Original discovery needs a valid SDK operation identity.');
      if(!issued.has(p.operationId))issued.set(p.operationId,'model-operation-'+(issued.size+1));p.operationId=issued.get(p.operationId);
    }
    store.trace.push(['rpc',method,p]);
    const failure=i.rpcFailure;
    if(failure&&failure.method===method&&(!failure.name||failure.name===params.namePrefix))throw new IvyError(failure.code,'Fixture discovery RPC failed.',failure.outcome);
    if(method==='serviceNodes.list')return structuredClone(i.nodePages[nodePage++]);
    if(method==='system.status')return {runtimeEpoch:'11111111-1111-4111-8111-111111111111'};
    if(method==='tools.list'){
      if(i.delayBinding===params.namePrefix)for(let n=0;n<6;n++)await Promise.resolve();
      if(i.bindingFailures?.[params.namePrefix])throw new IvyError(i.bindingFailures[params.namePrefix],'Fixture discovery RPC failed.');
      if(i.missingBinding===params.namePrefix)return {provider:{node:{serviceNodeId:params.serviceNodeId}},items:[],nextCursor:null};
      if(i.pagedBinding&&!params.cursor)return {provider:{node:{serviceNodeId:params.serviceNodeId}},items:[],nextCursor:'catalog-more'};
      const advertised=[i.node,...(i.nodePages??[]).flatMap(page=>page.items)].find(node=>node?.serviceNodeId===params.serviceNodeId);
      return {provider:{node:{serviceNodeId:i.provider??params.serviceNodeId}},items:[{qualifiedName:params.namespace+'.'+params.namePrefix,definition:{namespace:params.namespace,name:params.namePrefix,interfaceVersion:i.interfaceVersion??advertised.nativeVersion,annotations:{readOnlyHint:true},inputSchema:i.tierSchemas?.[params.namePrefix]??stringTier,outputSchema:{}},definitionHash:'sha256:'+(refreshed?'c':'a').repeat(64),catalogHash:'sha256:'+'b'.repeat(64)}],nextCursor:null};
    }
    if(method==='tools.call'){
      if(i.refresh&&!refreshed){refreshed=true;throw new IvyError('tool_definition_changed','Fixture changed read definition.');}
      const index=modelPages.get(params.serviceNodeId)??0;modelPages.set(params.serviceNodeId,index+1);
      const page=i.models[params.serviceNodeId][index];if(!page)throw Error('Missing fixture model page');
      if(page.error)throw new IvyError(page.error.code,'Fixture discovery RPC failed.',page.error.outcome);
      return structuredClone(page);
    }
    throw Error('Unexpected fixture method '+method);
  }};
  store.client=client;
  try{
    let value;
    if(row.method==='initial')value=functions.initialConfiguration();
    else if(row.method==='view')value=await functions.configurationView(store);
    else if(row.method==='merge')value=functions.executionWithDefaults(i.fields,i.defaults);
    else if(row.method==='effective')value=await functions.effectiveExecution(store,i.task);
    else if(row.method==='supports')value=await functions.supportsExecutionOptions(client,i.node,i.options);
    else value=await functions.checkConfiguration(store,i.value,i.serviceNodeId,i.allowUnavailable);
    row.expected={value:value??null,trace:store.trace,issued:issued.size};
  }catch(error){row.expected={value:{error:{code:error.code,message:error.message,outcome:error.outcome}},trace:store.trace,issued:issued.size};}
  // Independent shared-client discovery continues after a rejected Promise.all.
  for(let n=0;n<12;n++)await Promise.resolve();
}
// Keep fixture setup failures from turning successful catalog cases into false coverage.
const expected=name=>rows.find(row=>row.name===name).expected;
for(const name of ['model and effort','model id alias','reasoning chooses default model','later model page','complete snapshot 1000','paged snapshot 1000','exact tool on a later catalog page','changed read definition refresh preserves operation','fast checks both start schemas','flex checks both start schemas','nullable string tier supported','referenced tier schema supported'])assert.equal(expected('supports: '+name).value,true,name);
for(const name of ['unknown model','unsupported reasoning','missing default model','first matching alias is authoritative','missing thread tier refuses','missing turn tier refuses'])assert.equal(expected('supports: '+name).value,false,name);
for(const name of ['repeated model cursor refuses','longer model cursor cycle refuses','complete snapshot 1001','paged snapshot 1001'])assert.equal(expected('supports: '+name).value.error.code,'task_board_model_unavailable',name);
for(const name of ['ready registered selection','registered host later page','model discovery across eligible hosts'])assert.equal(expected('check: '+name).value,null,name);
assert.equal(expected('supports: original failed model call propagates').value.error.outcome,'unknown');
assert.equal(expected('supports: slow thread failure observes turn error first').value.error.code,'fixture_fast');
assert.equal(expected('supports: both tier lookups begin after first error').trace.filter(event=>event[0]==='rpc'&&event[1]==='tools.list').length,2);
assert.equal(expected('supports: changed read definition refresh preserves operation').issued,1);
process.stdout.write(JSON.stringify(rows));
