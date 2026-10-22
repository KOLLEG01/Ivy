import assert from 'node:assert/strict';
import {registerHooks} from 'node:module';
import {mkdtemp,writeFile,unlink,rmdir} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {dirname,join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {load,oracleRoot,settings as base} from './inbox-fixtures.mjs';
const {IvyError}=await load('packages/sdk/src/node.js');
const {loadServiceConfiguration}=await load('services/secretary/src/service-config.js');
const {minimumMessageAgeMinutes,loadMinimumMessageAgeMinutes}=await load('services/secretary/src/delivery-config.js');
const {triageConfiguration,loadTriageConfiguration}=await load('services/secretary/src/triage-config.js');
const {triageBundled}=await load('services/secretary/src/triage-schema.js');
const directory=await mkdtemp(join(tmpdir(),'secretary-config-')),file=join(directory,'config.json');
const settings={...base,identity:{...base.identity,serviceNodeId:'fixture.secretary'}};
const triage={schemaVersion:1,target:{serviceNodeId:'fixture.agent',hostId:'fixture-host',nativeVersion:'fixture',nativeExecutableHash:'sha256:'+'a'.repeat(64),catalogHash:'sha256:'+'b'.repeat(64)},model:'fixture',effort:'high',instructions:'Explicit fixture rules.',threadCwd:directory,maximumConcurrent:2};
const rows=[],capture=async(input,work)=>{try{rows.push({input,expected:{value:await work()}});}catch(cause){const e=IvyError.from(cause);rows.push({input,expected:{error:{code:e.code,message:e.message,outcome:e.outcome}}});}};
const invalid=()=>new IvyError('fixture_envelope_invalid','Protected configuration fixture is invalid.');
let running,hooks;
try{
 for(const value of [undefined,null,0,-0,1,5,1440,-1,1441,0.5,1439.5,Number.MAX_SAFE_INTEGER,true,false,'5','',[],{},NaN,Infinity,-Infinity]){
  const input={action:'minimum',...(value===undefined?{missing:true}:!Number.isFinite(value)&&typeof value==='number'?{specialNumber:String(value)}:{value})};
  await capture(input,()=>minimumMessageAgeMinutes(value));
 }
 // Reuse the existing triage contract test's invalid settings, with explicit
 // null/absence/path and structured-clone boundaries for native startup.
 for(const change of [{},{model:''},{effort:'auto'},{instructions:''},{threadCwd:'relative'},{maximumConcurrent:0},{maximumConcurrent:9},{target:{...triage.target,catalogHash:'unbound'}},{threadCwd:'/'},{threadCwd:'\\root-relative'},{threadCwd:'C:drive-relative'},{threadCwd:'C:\\absolute'},{threadCwd:'\\\\server\\share\\directory'},{threadCwd:directory+'/lone-\ud800'},{instructions:'A\ud800B'}]){
  const value={...structuredClone(triage),...change};
  await capture({action:'triage',value},()=>{const result=triageConfiguration(value,settings);assert.notEqual(result,value);assert.notEqual(result.target,value.target);return result;});
 }
 for(const value of [null,false,[],{},1,'triage'])await capture({action:'triage',value},()=>triageConfiguration(value,settings));
 const bodies=[undefined,null,false,[],{},1,'config',{schemaVersion:null},{schemaVersion:'1'},{schemaVersion:2},{schemaVersion:1},{schemaVersion:1,unrelated:{retained:'\ud800'}},{schemaVersion:1,minimumMessageAgeMinutes:null},{schemaVersion:1,minimumMessageAgeMinutes:0},{schemaVersion:1,minimumMessageAgeMinutes:1440},{schemaVersion:1,minimumMessageAgeMinutes:'5'},{schemaVersion:1,minimumMessageAgeMinutes:-1},{schemaVersion:1,triage:null},{schemaVersion:1,triage},{schemaVersion:1,triage:{...triage,threadCwd:'relative'}},{schemaVersion:1,triage:{...triage,maximumConcurrent:9}}];
 for(const body of bodies){
  if(body===undefined)await unlink(file).catch(error=>{if(error.code!=='ENOENT')throw error;});
  else await writeFile(file,JSON.stringify(body),{mode:0o600});
  for(const action of ['envelopeFile','deliveryFile','triageFile'])await capture({action,...(body===undefined?{missing:true}:{value:body})},()=>action==='envelopeFile'?loadServiceConfiguration(file,1024*1024,invalid):action==='deliveryFile'?loadMinimumMessageAgeMinutes(file):loadTriageConfiguration(file,settings));
 }
 for(const rawKind of ['syntax','bom','oversized']){
  await writeFile(file,rawKind==='syntax'?'{':rawKind==='bom'?'\ufeff{"schemaVersion":1}':' '.repeat(1024*1024+1),{mode:0o600});
  for(const action of ['envelopeFile','deliveryFile','triageFile'])await capture({action,rawKind},()=>action==='envelopeFile'?loadServiceConfiguration(file,1024*1024,invalid):action==='deliveryFile'?loadMinimumMessageAgeMinutes(file):loadTriageConfiguration(file,settings));
 }
 await unlink(file);
 for(const name of ['TriageSettings','TriageInput','TriageMediaAttachment','TriageMedia','TriageBinding','TriagePlan','TriageThread','TriageWork','TriageCall','TriageCompletion'])await capture({action:'bundle',name},()=>triageBundled(name));
 for(const dataRoot of [directory,join(directory,'data'),join(directory,'data')+'/',directory+'/data/../data',directory+'/lone-\ud800/data'])await capture({action:'path',dataRoot},()=>join(dirname(resolve(dataRoot)),'config.json'));
 const state={config:null};
 class Client{start(){}async stop(){}}
 class Health{async write(){}async control(){return null;}}
 globalThis.__secretaryConfigMocks={state,Client,Health};
 const mainUrl=pathToFileURL(join(oracleRoot,'services/secretary/src/main.js')).href,actualSdk=pathToFileURL(join(oracleRoot,'packages/sdk/src/node.js')).href;
 const replacements={
  '../../../packages/sdk/src/node.js':`export * from ${JSON.stringify(actualSdk)};export const ServiceClient=globalThis.__secretaryConfigMocks.Client;`,
  '../../../packages/sdk/src/host.js':'export const instanceConfig=async()=>globalThis.__secretaryConfigMocks.state.config;export const configurationPath=()=>"fixture";export const HealthFile=globalThis.__secretaryConfigMocks.Health;',
 };
 hooks=registerHooks({resolve(specifier,context,next){if(context.parentURL===mainUrl&&specifier in replacements)return{url:'secretary-config:'+encodeURIComponent(specifier),shortCircuit:true};return next(specifier,context);},load(url,context,next){if(url.startsWith('secretary-config:'))return{format:'module',source:replacements[decodeURIComponent(url.slice('secretary-config:'.length))],shortCircuit:true};return next(url,context);}});
 const {startSecretary}=await import(mainUrl);
 const instance={componentId:'secretary',credential:'fixture',serviceNodeId:'fixture.secretary',hostId:settings.identity.hostId,settings,dataRoot:join(directory,'data'),publicBaseUrl:'http://127.0.0.1:1',version:'fixture',buildId:'sha256:'+'a'.repeat(64)};
 for(const changes of [{},{componentId:'other'},{credential:''},{credential:null},{credential:false},{credential:0},{credential:[]},{credential:{}},{credential:'\ud800'},{serviceNodeId:'other'},{hostId:'other'},{settings:null},{settings:{}},{componentId:'other',settings:{}},{settings:{...settings,pollMs:0}},{settings:{...settings,identity:{...settings.identity,serviceNodeId:'other'}}},{settings:{...settings,identity:{...settings.identity,hostId:'other'}}}]){
  state.config={...structuredClone(instance),...changes};
  await capture({action:'startup',value:state.config},async()=>{running=await startSecretary('fixture');await running.close();running=null;return state.config.settings;});
 }
 process.stdout.write(JSON.stringify({platform:process.platform,rows})+'\n');
}finally{
 await running?.close();hooks?.deregister();delete globalThis.__secretaryConfigMocks;
 await unlink(file).catch(error=>{if(error.code!=='ENOENT')throw error;});await rmdir(directory);
}
