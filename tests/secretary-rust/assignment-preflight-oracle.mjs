import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire,syncBuiltinESMExports} from 'node:module';
import {promisify} from 'node:util';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
const require=createRequire(import.meta.url),child=require('node:child_process');
const originalExec=child.execFile;let processHandler;
const fakeExec=()=>assert.fail('Only the promised execFile boundary is expected');
fakeExec[promisify.custom]=(...args)=>processHandler(...args);
child.execFile=fakeExec;syncBuiltinESMExports();
const {load,settings,owner,rootId,itemId,at}=await import('./inbox-fixtures.mjs');
const {AssignmentRunner}=await load('services/secretary/src/assignment-runner.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {IvyError,canonical,hashJson,parseOperationId}=await load('packages/sdk/src/node.js');
const {secretaryNotificationPolicy}=await load('instructions/secretary-notification-policy.js');
const {Ajv2020}=require('ajv/dist/2020.js'),schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const seed=JSON.parse(schemaSamples(ajv,[schema]).find(r=>r.valid&&r.reference.endsWith('#/$defs/Execution')).input);
const catalog=JSON.parse(readFileSync('specs/native/codex-0.159.2/catalog.json','utf8'));
const target={serviceNodeId:'fixture.agent',hostId:'fixture-host',nativeVersion:catalog.version,nativeExecutableHash:catalog.nativeExecutableHash,catalogHash:hashJson(catalog)};
const rows=[];
async function add(label,c={}){
 const initial={...structuredClone(seed),phase:'preflight',serviceNodeId:target.serviceNodeId,nativeTarget:c.noTarget?null:{...target,...c.target},preflightOutput:'',...c.execution};
 initial.assignmentSnapshot.preflight=c.noCommand?null:{executable:'fixture-executable',args:['original','\ud800'],timeoutMs:1234};
 initial.executionTarget.threadCwd='/workspace';
 const current={pin:{objectId:itemId,revision:1},value:initial,writtenAt:at};
 let value=structuredClone(initial),revision=1,store,owners=0,clock=c.time??at,ran=false,processRequest=null;
 const trace=[],steps=[],controller=new AbortController();if(c.cancelBefore)controller.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 const normalized={code:'fixture_process_error',message:'Fixture process failed.',outcome:'unknown',...c.error};
 const failure=c.ivyError?new IvyError(normalized.code,normalized.message,normalized.outcome):new Error('Fixture process failed.');
 failure.stdout=c.stdout;failure.stderr=c.stderr;
 processHandler=async(executable,args,options)=>{
  ran=true;processRequest={executable,args,cwd:options.cwd,timeout:options.timeout,maxBuffer:options.maxBuffer,windowsHide:options.windowsHide,encoding:options.encoding,signalAborted:options.signal.aborted};
  clock=c.finishedAt??'2026-01-02T03:04:05.006Z';
  if(c.cancelProcess)controller.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
  if(c.failed)throw failure;
  return {stdout:c.stdout??'',stderr:c.stderr??''};
 };
 const read=()=>({object:{id:itemId,parentId:rootId,effectivelyArchived:false,contractKey:'secretary/execution'},revision:{revision,contractVersion:contractVersion('secretary/execution'),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:store.objectWriteRequest('secretary/execution',value,{object:{objectId:itemId,revision}},'fixture:1800000000000:read').references,createdAt:at},content:{encoding:'json',value:structuredClone(value)}});
 const client={async request(method,p,options){
  const copied=structuredClone(p);if(method==='objects.write'){parseOperationId(copied.mutationId);copied.mutationId='<scoped-operation>';}
  trace.push({method,params:copied,timeout:options?.timeoutMs??35000});let result,error;
  if(c.fail===method)error={code:'fixture_failure',message:'Fixture failure.',outcome:'unknown'};
  else if(method==='serviceNodes.get'){owners++;result={...owner,...(owners===c.ownerAt?c.owner:{})};}
  else if(method==='system.status')result={runtimeEpoch:'fixture'};
  else if(method==='objects.write'){assert.equal(p.expectedRevision,revision);value=structuredClone(p.content.value);revision++;result={object:{id:itemId},revision:{revision}};}
  else if(method==='objects.read')result=read();
  else if(method==='tools.list')result={provider:{node:{serviceNodeId:p.serviceNodeId}},items:[{qualifiedName:'agent.status',definition:{namespace:'agent',name:'status',interfaceVersion:'1.0.0',inputSchema:{},outputSchema:{}},definitionHash:'sha256:'+'a'.repeat(64),catalogHash:target.catalogHash}],nextCursor:null};
  else if(method==='tools.call'){assert.equal(p.qualifiedName,'agent.status');result={...target,epoch:'epoch',...c.status};}
  else assert.fail('Unexpected RPC '+method);
  if(error){steps.push({method,error});throw new IvyError(error.code,error.message,error.outcome);}
  steps.push({method,value:structuredClone(result)});return result;
 }};
 store=new SecretaryStore(client,rootId);
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings,owner:{serviceNodeId:'fixture.secretary'},signal:controller.signal,now:()=>new Date(clock)});
 let result;try{result={value:await new AssignmentRunner(engine).preflight(current)};}catch(cause){const e=IvyError.from(cause);result={code:e.code,message:e.message,outcome:e.outcome};}
 store.close();const e=IvyError.from(failure);
 rows.push({input:{label,current,steps,time:Date.parse(c.time??at),finishedAt:Date.parse(c.finishedAt??'2026-01-02T03:04:05.006Z'),cancelBefore:c.cancelBefore??false,cancelProcess:c.cancelProcess??false,failed:c.failed??false,ivyError:c.ivyError??false,stdout:c.stdout??null,stderr:c.stderr??null,error:{code:e.code,message:e.message,outcome:e.outcome}},expected:{result,trace,ran},processRequest});
}
await add('success-empty');await add('success-both',{stdout:'standard output',stderr:'standard error'});
await add('success-stderr',{stdout:'',stderr:'diagnostics'});
await add('success-whitespace',{stdout:' \n ',stderr:'\ufeff'});
await add('success-unicode',{stdout:'\ud800\0é😀',stderr:'\udc00'});
for(const length of [65535,65536,65537])await add('success-length-'+length,{stdout:'x'.repeat(length)});
await add('success-split-surrogate',{stdout:'x'.repeat(65535)+'😀'});
await add('success-joined-boundary',{stdout:'x'.repeat(65535),stderr:'tail'});
await add('no-command',{noCommand:true,noTarget:true,cancelBefore:true});
await add('target-status',{noTarget:true,stdout:'done'});
await add('target-mismatch',{target:{hostId:'other'}});
await add('target-not-ready',{noTarget:true,status:{epoch:null}});
await add('success-keeps-started-at',{stdout:'done',execution:{startedAt:'2020-01-01T00:00:00.000Z'}});
await add('success-sets-started-at',{stdout:'done',execution:{startedAt:null}});
for(const ivyError of [false,true]){
 await add('failure-empty-'+ivyError,{failed:true,ivyError});
 await add('failure-both-'+ivyError,{failed:true,ivyError,stdout:' stdout ',stderr:' stderr '});
 await add('failure-keeps-time-'+ivyError,{failed:true,ivyError,execution:{startedAt:null}});
 await add('failure-cancelled-'+ivyError,{failed:true,ivyError,cancelProcess:true,stdout:'must not save'});
}
for(const text of ['', ' \t\r\n', '\ufeff', '\u0085', '\u200b', '\ud800', '\udc00'])await add('failure-whitespace-'+rows.length,{failed:true,stderr:text,stdout:' \t '});
for(const values of [[{},[]],[null,42],[false,' kept '],[' retained ',{}]])await add('failure-non-strings-'+rows.length,{failed:true,stdout:values[0],stderr:values[1]});
await add('failure-length',{failed:true,stderr:'e'.repeat(65537),stdout:'tail'});
await add('failure-split-surrogate',{failed:true,stderr:'x'.repeat(65535)+'😀'});
await add('success-cancelled',{cancelProcess:true,stdout:'must not save'});
await add('cancel-before',{cancelBefore:true});
for(const ownerAt of [1,2])await add('owner-changed-'+ownerAt,{ownerAt,owner:{principalId:'other'},stdout:'output'});
for(const fail of ['objects.write','objects.read','serviceNodes.get','system.status'])await add('rpc-'+fail,{fail,stdout:'output'});
child.execFile=originalExec;syncBuiltinESMExports();
process.stdout.write(JSON.stringify({settings,rootId,notificationPolicy:secretaryNotificationPolicy,rows}));
