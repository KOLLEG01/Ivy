import {registerHooks} from 'node:module';
import assert from 'node:assert/strict';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {readNativeContract}=await import('../../packages/sdk/src/native.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
const base=JSON.parse(readFileSync('specs/native/codex-0.159.2/catalog.json','utf8'));
const catalog={...base,clientRequests:[{...base.clientRequests[0],inputSchema:true,outputSchema:true}],serverRequests:[],serverNotifications:[],clientNotifications:[],derivedLegacyTypes:[]};
const target={serviceNodeId:'native-agent',hostId:'fixture',nativeVersion:catalog.version,nativeExecutableHash:catalog.nativeExecutableHash,catalogHash:hashJson(catalog)};
const status={...target,epoch:'epoch',pid:42,state:'ready',observedAt:'2026-01-02T03:04:05.006Z',code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
const rows=[],cancelError=new IvyError('fixture_cancelled','Fixture cancellation.');
const defaults=()=>({target:structuredClone(target),steps:[{value:structuredClone(status)},{value:structuredClone(catalog)},{value:structuredClone(status)}]});
async function collect(label,edit=()=>{}){
 const input=defaults();edit(input);const controller=new AbortController(),trace=[];let calls=0;
 if(input.cancelBefore)controller.abort(cancelError);
 const client={async request(method,params,options){
  trace.push({method,params,...(method==='tools.call'?{timeout:options.timeoutMs}:{})});
  if(method==='tools.list'){
   if(input.failList)throw new IvyError('fixture_discovery','Discovery failed.');
   return {provider:{node:{serviceNodeId:'native-agent'}},items:['status','catalog'].map(name=>({qualifiedName:'agent.'+name,definition:{namespace:'agent',name,interfaceVersion:'1.0.0',annotations:{readOnlyHint:true},inputSchema:{},outputSchema:{}},definitionHash:'sha256:'+'a'.repeat(64),catalogHash:'sha256:'+'b'.repeat(64)})),nextCursor:null};
  }
  if(method!=='tools.call')throw Error('Unexpected method');
  const step=input.steps[calls++];if(input.cancelAfter===calls)controller.abort(cancelError);
  if(step.error)throw new IvyError(step.error.code,step.error.message,step.error.outcome);
  return structuredClone(step.value);
 }};
 let expected;try{const result=await readNativeContract(client,input.target,{timeoutMs:1234,signal:controller.signal});expected={ok:true,value:{target:result.target,epoch:result.epoch,catalogHash:result.contract.catalogHash,definitions:[...result.contract.definitions().keys()]}};}catch(e){expected={ok:false,error:IvyError.from(e).toWire()};}
 rows.push({label,input:JSON.stringify(input),expected:JSON.stringify(expected),trace:JSON.stringify(trace)});
}
await collect('valid');await collect('optional-host',i=>delete i.target.hostId);
for(const index of [0,2])for(const [key,value] of [['serviceNodeId','other'],['hostId','other'],['state','stopped'],['epoch',null],['epoch','other'],['nativeVersion','0.154.0'],['catalogHash','sha256:'+'f'.repeat(64)],['nativeExecutableHash','sha256:'+'f'.repeat(64)],['observedAt','2026-02-30T03:04:05.006Z'],['observedAt','invalid'],['pid',-1]])await collect('status-'+index+'-'+key,i=>i.steps[index].value[key]=value);
for(const value of [null,{},[],{...catalog,clientRequests:[catalog.clientRequests[0],catalog.clientRequests[0]]},{...catalog,version:'0.154.0'},{...catalog,sourceHash:'sha256:'+'f'.repeat(64)},{...catalog,nativeExecutableHash:'sha256:'+'f'.repeat(64)}])await collect('catalog-invalid-or-mismatch',i=>i.steps[1].value=value);
for(const field of ['nativeVersion','nativeExecutableHash','catalogHash'])await collect('explicit-catalog-'+field,i=>{
 i.target[field]=field==='nativeVersion'?'0.154.0':'sha256:'+'f'.repeat(64);
 for(const index of [0,2])i.steps[index].value[field]=i.target[field];
});
for(const index of [0,1,2])for(const outcome of ['not_executed','unknown','completed'])await collect('request-error',i=>i.steps[index]={error:{code:'fixture_failure',message:'Provider failure.',outcome}});
await collect('discovery-error',i=>i.failList=true);await collect('cancel-before',i=>i.cancelBefore=true);
for(const index of [1,2,3])await collect('cancel-after-'+index,i=>i.cancelAfter=index);
await collect('catalog-error-before-after-status',i=>{i.steps[1].value={};i.steps[2].value={};});
await collect('status-error-before-epoch-change',i=>{i.steps[2].value.state='stopped';i.steps[2].value.epoch='changed';});
await collect('utf16-epoch',i=>{i.steps[0].value.epoch='\ud800';i.steps[2].value.epoch='\ud800';});
assert.equal(JSON.parse(rows[0].expected).ok,true,'valid fixture must reach a snapshot');
assert.equal(JSON.parse(rows[0].trace).filter(r=>r.method==='tools.call').length,3);
for(const code of ['native_catalog_mismatch','native_epoch_changed','fixture_cancelled','fixture_failure','fixture_discovery'])assert(rows.some(row=>JSON.parse(row.expected).error?.data.code===code),`missing ${code} coverage`);
process.stdout.write(JSON.stringify(rows));
