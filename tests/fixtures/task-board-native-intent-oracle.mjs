import {existsSync,readFileSync,readdirSync} from 'node:fs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
registerHooks({resolve(name,context,next){
  if(name.startsWith('.')&&name.endsWith('.js')&&context.parentURL){const u=new URL(name.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(u)))return next(u.href,context);}
  return next(name,context);
}});
const {checkedNativeContract}=await import('../../packages/contracts/src/checked-native-contract.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {validateTaskBoard}=await import('../../packages/contracts/src/task-board-validation.ts');
const {readNativePlan,saveNativePlan,validateNativeInvocation}=await import('../../packages/sdk/src/native-plan.ts');
const {readNativeParameters,saveNativeParameters}=await import('../../packages/sdk/src/native-parameters.ts');
const load=name=>readFileSync('services/task-board/src/runtime/'+name+'.ts','utf8');
const mutationText=load('store').match(/export const mutation = [\s\S]*?;/)[0].replace('export ','');
const mutation=new Function('hashJson',stripTypeScriptTypes(mutationText)+'\nreturn mutation;')(hashJson);
const plans=new Function('checkedNativeContract','readNativePlan',stripTypeScriptTypes(load('native-plan').replace(/^import .*;\r?\n/gm,'').replaceAll('export ',''))+'\nreturn {resolvedPlan,readPlan};')(checkedNativeContract,readNativePlan);
let trace;
const validate=(name,value)=>{trace.push(['validate',name]);validateTaskBoard(name,value);};
const intentSource=load('native-intent').slice(load('native-intent').indexOf('export async function retainNativeRequest')).replaceAll('export ','');
const intent=new Function('checkedNativeContract','validateNativeInvocation','saveNativeParameters','readNativeParameters','mutation','validateTaskBoard',stripTypeScriptTypes(intentSource)+'\nreturn {retainNativeRequest,resolveNativeRequest,evidenceIdentity};')
  (checkedNativeContract,validateNativeInvocation,saveNativeParameters,readNativeParameters,mutation,validate);
const pin={objectId:'saved-plan',revision:1},zero='sha256:'+'0'.repeat(64),groups=[];
for(const directory of readdirSync('specs/native').filter(name=>/^codex-/.test(name))){
  const path='specs/native/'+directory+'/catalog.json';if(!existsSync(path))continue;
  const catalog=JSON.parse(readFileSync(path,'utf8')),source=checkedNativeContract(catalog.version,{sourceHash:catalog.sourceHash});
  const identity={nativeVersion:catalog.version,catalogSourceHash:catalog.sourceHash};
  const draft={...identity,definitions:Object.fromEntries(Object.entries({threadStart:'thread/start',threadResume:'thread/resume',turnStart:'turn/start',turnInterrupt:'turn/interrupt'}).map(([key,method])=>[key,hashJson(source.definitions.get(method))])),threadStart:{},threadResume:{},turnStart:{input:[]}};
  const client={request(){throw Error('Inline native helpers must not make RPC calls');}};
  const plan=await saveNativePlan(client,source.contract,{draft,kind:'task-board',parentId:'root',mutationId:'plan'});
  const request={...identity,method:'turn/interrupt',params:{threadId:'thread',turnId:'turn'}};
  const root={rootObjectId:'root',client};trace=[];
  const retained=await intent.retainNativeRequest(root,request,'native-operation');
  const base={plan,request,retained,run:{target:{serviceNodeId:'native-agent'}},call:{operationId:'native-operation',request:retained}};
  const rows=[],add=(name,action,input={})=>rows.push({name,action,input});
  add('exact resolved plan','resolved');add('exact stored plan','read');
  add('inline references are independent of root','resolved',{root:'another-root'});add('null root retains inline format','resolved',{root:null});
  for(const nativeVersion of ['unsupported','',null,'bad\ud800'])add('plan version '+JSON.stringify(nativeVersion),'resolved',{plan:{...plan,nativeVersion}});
  for(const catalogSourceHash of [zero,null,'bad'])add('plan catalog '+catalogSourceHash,'resolved',{plan:{...plan,catalogSourceHash}});
  add('plan source selected before draft rejection','resolved',{plan:{...plan,catalogSourceHash:zero,definitions:{}}});
  add('saved definition mismatch','resolved',{plan:{...plan,definitions:{...plan.definitions,turnStart:zero}}});
  add('parameter reference method mismatch','resolved',{plan:{...plan,threadResume:{...plan.threadResume,method:'turn/interrupt'}}});
  add('all inline reads preserve first failure','resolved',{plan:{...plan,threadStart:{...plan.threadStart,catalogHash:zero},threadResume:{...plan.threadResume,method:'turn/interrupt'}}});
  add('retain exact invocation','retain');add('retain has no workflow writes','retain',{root:null});add('retain preserves UTF16 operation identity','retain',{operation:'native\ud800'});
  for(const nativeVersion of ['unsupported',null])add('retain version '+nativeVersion,'retain',{request:{...request,nativeVersion}});
  add('catalog selection before input validation','retain',{request:{...request,catalogSourceHash:zero,params:null}});
  add('retain exact method admission','retain',{request:{...request,method:'unknown'}});
  add('retain invalid native parameters','retain',{request:{...request,params:{threadId:'thread'}}});
  add('retain UTF16 request values','retain',{request:{...request,params:{threadId:'thread\ud800',turnId:'turn\udfff'}}});
  add('retain preflight byte boundary','retain',{large:true});
  add('resolve exact invocation','resolve');add('resolve does not use parent for authorization','resolve',{root:'other'});
  for(const nativeVersion of ['unsupported','',null])add('request validation precedes catalog '+nativeVersion,'resolve',{retained:{...retained,nativeVersion}});
  for(const key of ['catalogHash','nativeExecutableHash'])add('reference identity '+key,'resolve',{retained:{...retained,params:{...retained.params,[key]:zero}}});
  add('reference method identity','resolve',{retained:{...retained,params:{...retained.params,method:'thread/resume'}}});
  add('reference reserved fields identity','resolve',{retained:{...retained,params:{...retained.params,reservedFields:['threadId']}}});
  add('reference params validated','resolve',{retained:{...retained,params:{...retained.params,params:{threadId:'thread'}}}});
  add('outer request refuses extra fields','resolve',{retained:{...retained,extra:true}});
  add('outer request refuses absent params','resolve',{retained:{...retained,params:null}});
  add('evidence identity uses resolved exact params','identity');
  add('evidence caller and selected owner stay exact','identity',{principal:'caller\ud800',run:{target:{serviceNodeId:'owner\udfff'}}});
  add('evidence identity refuses changed parameter catalog','identity',{call:{...base.call,request:{...retained,params:{...retained.params,catalogHash:zero}}}});
  for(const row of rows){
    trace=[];const i=row.input;
    const store={rootObjectId:i.root===undefined?'root':i.root,client,async read(key,address){assert.equal(key,'task-board/native-plan');assert.deepEqual(address,pin);validate('NativePlan',i.plan??plan);return {value:i.plan??plan};}};
    try{
      let value;
      if(row.action==='resolved')value=await plans.resolvedPlan(store,i.plan??plan);
      else if(row.action==='read')value=await plans.readPlan(store,pin);
      else if(row.action==='retain'){const invocation=structuredClone(i.request??request);if(i.large)invocation.params.extra='x'.repeat(1048576);value=await intent.retainNativeRequest(store,invocation,i.operation??'native-operation');}
      else if(row.action==='resolve')value=await intent.resolveNativeRequest(store,i.retained??retained);
      else value=await intent.evidenceIdentity(store,i.run??base.run,i.call??base.call,i.principal??'caller');
      row.expected={value,trace};
    }catch(error){row.expected={error:IvyError.from(error).toWire(),trace};}
  }
  for(const name of ['exact resolved plan','exact stored plan','retain exact invocation','resolve exact invocation','evidence identity uses resolved exact params'])assert.ok(rows.find(row=>row.name===name).expected.value,name);
  assert.equal(rows.find(row=>row.name==='reference identity catalogHash').expected.error.data.code,'native_parameters_contract_mismatch');
  assert.equal(rows.find(row=>row.name==='outer request refuses extra fields').expected.error.data.code,'invalid_arguments');
  groups.push({path,base,pin,rows});
}
assert.equal(groups.length,4);
process.stdout.write(JSON.stringify(groups));
