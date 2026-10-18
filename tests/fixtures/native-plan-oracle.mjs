import {readFileSync,readdirSync,existsSync} from 'node:fs';
import {registerHooks,createRequire} from 'node:module';
import {fileURLToPath,pathToFileURL} from 'node:url';
const require=createRequire(import.meta.url);
const external=new Map(['ajv/dist/2020.js','re2js'].map(name=>[name,pathToFileURL(require.resolve(name)).href]));
registerHooks({resolve(name,context,next){
  if(external.has(name))return next(external.get(name),context);
  if(name.startsWith('.')&&name.endsWith('.js')&&context.parentURL){const source=new URL(name.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(name,context);
}});
const {NativeContract}=await import('../../packages/contracts/src/native-contract.ts');
const {validateNativeInvocation,validateNativePlanDraft,saveNativePlan,readNativePlan}=await import('../../packages/sdk/src/native-plan.ts');
const {saveNativeParameters,readNativeParameters}=await import('../../packages/sdk/src/native-parameters.ts');
const {validateNativePayload}=await import('../../packages/sdk/src/native-evidence.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const zero='sha256:'+'0'.repeat(64),client={request(){throw Error('Inline parameters must not perform RPC');}};
const methods={threadStart:'thread/start',threadResume:'thread/resume',turnStart:'turn/start',turnInterrupt:'turn/interrupt'};
const groups=[];
function expand(input){
 const value=structuredClone(input);
 if(value.generated==='large-draft')value.value.threadStart.extra='x'.repeat(value.units);
 if(value.generated==='large-invocation')value.value.params.extra='x'.repeat(value.units);
 if(value.generated==='large-payload')value.value.params.body='x'.repeat(value.units);
 return value;
}
async function execute(contract,spec){
 const input=expand(spec),method=input.method??'turn/interrupt',reserved=input.reserved??[],parentId=input.parentId??null;
 switch(input.kind){
  case'input':contract.validateInput(method,input.value,input.maximumBytes);break;
  case'template':contract.validateTemplate(method,input.value,reserved,input.maximumBytes);break;
  case'result':contract.validateResult(method,input.value,input.maximumBytes);break;
  case'invocation':validateNativeInvocation(contract,input.value,input.maximumBytes);break;
  case'draft':validateNativePlanDraft(contract,input.value,input.planKind??'chat');break;
  case'params-save':return await saveNativeParameters(client,contract,{method,params:input.value,reserved,parentId,mutationId:'parameters'},{});
  case'params-read':return await readNativeParameters(client,contract,{method,ref:input.value,reserved,parentId},{});
  case'plan-save':return await saveNativePlan(client,contract,{draft:input.value,kind:input.planKind??'chat',parentId,mutationId:input.mutationId??'plan'},{});
  case'plan-read':return await readNativePlan(client,contract,{plan:input.value,kind:input.planKind??'chat',parentId},{});
  case'payload':validateNativePayload(contract,input.value,{requestBytes:input.requestBytes??6291456,responseBytes:input.responseBytes??25165824});break;
  default:throw Error('Unknown oracle action');
 }
 return null;
}
async function collect(catalog,source){
 const contract=new NativeContract(catalog),rows=[];
 const add=async(name,input)=>{
  let expected;try{expected={ok:true,value:await execute(contract,input)};}catch(error){expected={ok:false,error:IvyError.from(error).toWire()};}
  rows.push({name,input:JSON.stringify(input),expected:JSON.stringify(expected)});return expected;
 };
 const identity={nativeVersion:catalog.version,catalogSourceHash:catalog.sourceHash};
 const definitions=Object.fromEntries(Object.entries(methods).map(([key,method])=>[key,contract.definitionHash(method)]));
 const draft={...identity,definitions,threadStart:{},threadResume:{},turnStart:{}};
 const invocation={...identity,method:'turn/interrupt',params:{threadId:'thread',turnId:'turn'}};
 for(const method of ['thread/start','thread/resume','turn/start','turn/interrupt','unknown'])for(const value of [null,{},false,[],{threadId:'thread'},{threadId:'thread',turnId:'turn'},{threadId:'thread',input:[]},{threadId:'thread',input:[],extra:1},{extra:{nested:true}},{cwd:'\ud800'}]){
  await add('native input',{kind:'input',method,value});
  await add('native result',{kind:'result',method,value});
 }
 for(const value of [invocation,null,{}, {...invocation,nativeVersion:'other'}, {...invocation,catalogSourceHash:zero},{...invocation,method:'unknown'}, {...invocation,params:{threadId:'thread'}},{...invocation,extra:true},{...invocation,params:{threadId:'thread',turnId:'turn',extra:true}}])await add('invocation',{kind:'invocation',value});
 for(const maximumBytes of [0,1,10,64,256])await add('input byte precedence',{kind:'input',value:{threadId:'thread',turnId:'turn',extra:1},maximumBytes});
 await add('invocation catalog before size',{kind:'invocation',value:{...invocation,nativeVersion:'other'},maximumBytes:0});
 for(const planKind of ['chat','task-board']){
  const valid={...draft,turnStart:planKind==='chat'?{}:{input:[]}};
  const drafts=[valid,{...valid,location:{cwd:'C:/project'},threadStart:{cwd:'C:/project'}},{...valid,location:{cwd:'C:/project'}},{...valid,location:{cwd:'C:/project'},threadStart:{cwd:'other'}},{...valid,threadResume:{threadId:'reserved'}},{...valid,threadResume:{history:[]}},{...valid,turnStart:{threadId:'reserved'}},{...valid,turnStart:{input:[]}},{...valid,threadStart:{extra:1}},{...valid,definitions:{...definitions,threadStart:zero}},{...valid,nativeVersion:'other'}];
  for(const value of drafts){await add('draft '+planKind,{kind:'draft',value,planKind});await add('save '+planKind,{kind:'plan-save',value,planKind,parentId:'workflow'});}
  const saved=await execute(contract,{kind:'plan-save',value:valid,planKind});
  await add('read exact inline plan',{kind:'plan-read',value:saved,planKind,parentId:'another-workflow'});
  for(const key of ['nativeVersion','catalogHash','nativeExecutableHash','method','reservedFields','params']){
   const changed=structuredClone(saved);changed.threadStart[key]=key==='reservedFields'?['extra']:key==='params'?{extra:1}:key==='method'?'turn/interrupt':key==='nativeVersion'?'other':zero;
   await add('read changed '+key,{kind:'plan-read',value:changed,planKind});
  }
  const two=structuredClone(saved);two.threadStart.catalogHash=zero;two.threadResume.method='turn/interrupt';await add('all reads before first rejection',{kind:'plan-read',value:two,planKind});
 }
 for(const reserved of [[],['threadId','turnId'],['turnId','threadId','threadId'],['\ud800','😀','threadId']])for(const value of [{},{threadId:'thread',turnId:'turn'},{extra:true}]){
  await add('parameter projection',{kind:'params-save',method:'turn/interrupt',value,reserved});
  await add('template projection',{kind:'template',method:'turn/interrupt',value,reserved});
 }
 const reference=await execute(contract,{kind:'params-save',method:'turn/interrupt',value:{threadId:'thread',turnId:'turn'}});
 for(const value of [reference,{...reference,schemaVersion:2},{...reference,reservedFields:['b','a']},{...reference,params:null},{...reference,catalogHash:zero},{...reference,extra:true},null])await add('parameter read',{kind:'params-read',method:'turn/interrupt',value});
 const observed={method:'turn/interrupt',params:{threadId:'thread',turnId:'turn'},requestId:'request'};
 for(const reply of [undefined,null,false,0,'','text',true,[],{}, {result:{}},{error:{code:-1,message:'failed'}},{id:'override',result:{}},{result:{extra:1}},{result:null},{result:9007199254740992}])for(const requestId of ['request',null,undefined])await add('payload envelope',{kind:'payload',value:{...observed,requestId,reply}});
 for(const requestBytes of [0,1,64,128])for(const responseBytes of [0,1,24,128])await add('payload byte order',{kind:'payload',value:{...observed,reply:{result:{}}},requestBytes,responseBytes});
 for(const value of [null,{},false,[],{method:17,params:{}},{method:'unknown',params:{}},{...observed,params:{extra:true},reply:{result:null}}])await add('payload malformed',{kind:'payload',value});
 for(const generated of ['large-draft','large-invocation'])await add('bounded preflight '+generated,{kind:generated==='large-draft'?'draft':'invocation',value:generated==='large-draft'?draft:invocation,generated,units:1048576});
 if(source.label){
  for(const units of [6291356,6291456])await add('real request frame bound',{kind:'payload',generated:'large-payload',units,value:{method:'body/read',params:{},requestId:'request'}});
 }
 groups.push({...source,rows});
}
for(const directory of readdirSync('specs/native').filter(name=>/^codex-/.test(name))){const path='specs/native/'+directory+'/catalog.json';if(existsSync(path))await collect(JSON.parse(readFileSync(path,'utf8')),{path});}
const base=JSON.parse(readFileSync('specs/native/codex-0.159.2/catalog.json','utf8'));
const extras={type:'object',properties:{threadId:{type:'string'},turnId:{type:'string'},input:{type:'array'},cwd:{type:['string','null']},history:{},path:{},clientUserMessageId:{},extension:{type:'object',additionalProperties:true}}};
const catalog={...base,clientRequests:[...Object.values(methods).map(method=>({...base.clientRequests[0],method,inputSchema:structuredClone(extras),outputSchema:{type:'object',properties:{}}})),{...base.clientRequests[0],method:'body/read',inputSchema:{type:'object',properties:{body:{type:'string'}},required:['body']},outputSchema:true}]};
await collect(catalog,{label:'synthetic native extension roots',source:JSON.stringify(catalog)});
process.stdout.write(JSON.stringify(groups));
