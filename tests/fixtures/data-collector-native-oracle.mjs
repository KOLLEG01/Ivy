import {readFileSync,existsSync} from 'node:fs';
import {stripTypeScriptTypes,registerHooks,createRequire} from 'node:module';
import {fileURLToPath,pathToFileURL} from 'node:url';
const require=createRequire(process.env.IVY_TEST_PACKAGE_JSON||import.meta.url);
registerHooks({resolve(s,c,n){if(s==='ajv'||s==='ws')return n(pathToFileURL(require.resolve(s)).href,c);if(s.startsWith('.')&&s.endsWith('.js')){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {NativeOwner}=await import('../../packages/sdk/src/native-owner.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {newOperationId,responseValue,serviceTools}=await import('../../packages/sdk/src/client.ts');
const {validateAgent}=await import('../../packages/contracts/src/agent-validation.ts');
globalThis.__collectorNative={NativeOwner,IvyError,newOperationId,responseValue,serviceTools,validateAgent};
const js=stripTypeScriptTypes(readFileSync(new URL('../../services/data-collector/src/native-mcp.ts',import.meta.url),'utf8'),{mode:'transform'}).replace(/import\s*\{[^}]+\}\s*from\s*"[^\"]*packages\/sdk\/src\/node\.js";/,'const {NativeOwner,IvyError,newOperationId,responseValue,serviceTools,validateAgent}=globalThis.__collectorNative;');
const {NativeMcpSession}=await import('data:text/javascript;base64,'+Buffer.from(js).toString('base64'));
const digest=c=>'sha256:'+c.repeat(64),time='2026-01-02T03:04:05.006Z';
const target={serviceNodeId:'native-agent',hostId:'fixture',nativeVersion:'0.159.2',nativeExecutableHash:digest('a'),catalogHash:digest('c')};
const status={...target,epoch:'epoch',pid:42,state:'ready',observedAt:time,code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
const rows=[];
for(const kind of ['normal','concurrent','ungranted','relative','not-ready','bad-ephemeral','failed-start','changed-epoch','wrong-caller','uncertain','pending','absent-once','tool-error','cleanup-failure','cancelled']){
 const trace=[],ops=new Map(),controller=new AbortController();let polls=0;
 if(kind==='cancelled')controller.abort();
 const configuration={serviceNodeId:target.serviceNodeId,cwd:kind==='relative'?'relative':process.platform==='win32'?'C:/workspace':'/workspace',server:'fixture',tools:['echo','second']};
 function operation(call){
  const method=call.method;let result=method==='thread/start'?{thread:{id:'thread',ephemeral:kind!=='bad-ephemeral'}}:method==='thread/unsubscribe'?null:kind==='tool-error'?{isError:true,structuredContent:{error:{code:-32000,message:'private diagnostic',data:{code:'provider_unavailable',outcome:'unknown'}}}}:{structuredContent:{tool:call.params.tool,args:call.params.arguments}};
  return {schemaVersion:1,operationId:call.operationId,callerPrincipalId:kind==='wrong-caller'?'other':'caller',serviceNodeId:target.serviceNodeId,nativeVersion:target.nativeVersion,nativeExecutableHash:target.nativeExecutableHash,method,params:call.params,requestHash:hashJson({method,params:call.params}),createdAt:time,updatedAt:time,phase:kind==='failed-start'&&method==='thread/start'?'failed':'succeeded',epoch:kind==='changed-epoch'?'other':'epoch',requestId:1,reply:{result},code:null};
 }
 const client={async request(method,params){
  trace.push({method,params:structuredClone(params)});
  if(method==='system.status')return {runtimeEpoch:'hive-epoch',callerPrincipalId:'caller'};
  if(method==='tools.list'){
   const namespace=params.namespace,name=params.namePrefix;
   return {provider:{node:{serviceNodeId:target.serviceNodeId}},items:[{qualifiedName:namespace+'.'+name,definition:{namespace,name,interfaceVersion:namespace==='agent'?'1.0.0':target.nativeVersion,inputSchema:{},outputSchema:{}},definitionHash:digest(namespace==='agent'?'a':'b'),catalogHash:digest('c')}],nextCursor:null};
  }
  if(method!=='tools.call')throw Error(method);
  if(params.qualifiedName==='agent.status')return {...status,state:kind==='not-ready'?'stopped':'ready'};
  if(params.qualifiedName==='agent.invoke'){
   const call=params.arguments;if(call.method==='thread/unsubscribe'&&kind==='cleanup-failure')throw new IvyError('fixture_cleanup','Failed.');
   const op=operation(call);ops.set(call.operationId,op);
   if(call.method==='thread/start'&&kind==='uncertain')throw new IvyError('fixture_transport','Uncertain.','unknown');
   if(call.method==='thread/start'&&['pending','absent-once'].includes(kind))return {...op,phase:'accepted',epoch:null,requestId:null,reply:null};
   return op;
  }
  if(params.qualifiedName==='agent.operation'){
   if(kind==='absent-once'&&polls++===0)throw new IvyError('not_found','Not observed.','not_executed',{kind:'agent_operation_absent',operationId:params.arguments.operationId,serviceNodeId:target.serviceNodeId,epoch:'epoch'});
   return ops.get(params.arguments.operationId);
  }
  throw Error(params.qualifiedName);
 }};
 let session,value=null,error=null;
 try{session=new NativeMcpSession(client,configuration,controller.signal);value=kind==='concurrent'?await Promise.all([session.call('echo',{n:1}),session.call('second',{n:2})]):await session.call(kind==='ungranted'?'missing':'echo',{n:1});}
 catch(e){const failure=IvyError.from(e);error={code:failure.code,message:failure.message,outcome:failure.outcome};}
 if(session){await session.close();await session.close();}
 const ids=new Map();const normalize=v=>{if(Array.isArray(v))return v.map(normalize);if(v&&typeof v==='object')return Object.fromEntries(Object.entries(v).map(([k,v])=>[k,k==='operationId'&&typeof v==='string'?(ids.has(v)?ids.get(v):(ids.set(v,'operation-'+(ids.size+1)),ids.get(v))):normalize(v)]));return v;};
 rows.push({kind,configuration,target,status,value,error,trace:normalize(trace)});
}
process.stdout.write(JSON.stringify(rows));
