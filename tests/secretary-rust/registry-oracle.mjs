import assert from 'node:assert/strict';
import {registerHooks} from 'node:module';
import {mkdtemp,writeFile,unlink,rmdir} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {load,oracleRoot,settings as base,rootId} from './inbox-fixtures.mjs';
const {secretaryRegistry}=await load('services/secretary/src/schema.js');
const {triageRegistry}=await load('services/secretary/src/triage-schema.js');
const actualSdk=pathToFileURL(join(oracleRoot,'packages/sdk/src/node.js')).href;
const mainUrl=pathToFileURL(join(oracleRoot,'services/secretary/src/main.js')).href;
const state={options:null,config:null};
class Client {constructor(options){state.options=options;}start(){}async stop(){}}
class Health {async write(){}async control(){return null;}}
globalThis.__secretaryRegistryMocks={state,Client,Health};
const modules={
 '../../../packages/sdk/src/node.js':`export * from ${JSON.stringify(actualSdk)};export const ServiceClient=globalThis.__secretaryRegistryMocks.Client;`,
 '../../../packages/sdk/src/host.js':'export const instanceConfig=async()=>globalThis.__secretaryRegistryMocks.state.config;export const configurationPath=()=>"fixture-config";export const HealthFile=globalThis.__secretaryRegistryMocks.Health;',
};
const hooks=registerHooks({resolve(specifier,context,next){if(context.parentURL===mainUrl&&specifier in modules)return{url:'secretary-registry:'+encodeURIComponent(specifier),shortCircuit:true};return next(specifier,context);},load(url,context,next){if(url.startsWith('secretary-registry:'))return{format:'module',source:modules[decodeURIComponent(url.slice('secretary-registry:'.length))],shortCircuit:true};return next(url,context);}});
const directory=await mkdtemp(join(tmpdir(),'secretary-registry-')),rows=[];
try{
 const {startSecretary}=await import(mainUrl);
 const triage={schemaVersion:1,target:{serviceNodeId:'fixture.agent',hostId:'fixture-host',nativeVersion:'fixture',nativeExecutableHash:'sha256:'+'a'.repeat(64),catalogHash:'sha256:'+'b'.repeat(64)},model:'fixture',effort:'medium',instructions:'Explicit fixture rules.',threadCwd:directory,maximumConcurrent:1};
 for(const root of [rootId,'root-é😀','root-\ud800'])for(const configured of [false,true]){
  const settings={...base,identity:{...base.identity,serviceNodeId:'fixture.secretary',scope:{...base.identity.scope,rootObjectId:root}}};
  state.config={componentId:'secretary',credential:'fixture',serviceNodeId:'fixture.secretary',hostId:settings.identity.hostId,settings,dataRoot:join(directory,'data'),publicBaseUrl:'http://127.0.0.1:1',version:'fixture',buildId:'sha256:'+'a'.repeat(64)};
  await writeFile(join(directory,'config.json'),JSON.stringify({schemaVersion:1,...(configured?{triage}:{})}),{mode:0o600});
  const running=await startSecretary('fixture-config');
  try{rows.push({root,triageConfigured:configured,registry:state.options.registry()});}finally{await running.close();}
 }
 const baseline=secretaryRegistry(),dormant=triageRegistry();
 assert.equal(baseline.contracts.length,6);assert.equal(baseline.namespaces[0].tools.length,16);assert.equal(rows.length,6);
 assert.deepEqual(dormant,{namespaces:[],contracts:[],requiredContracts:[]});
 assert.deepEqual(rows[0].registry,rows[1].registry,'Configured dormant triage must add no contracts');
 process.stdout.write(JSON.stringify({baseline,dormant,rows}));
}finally{hooks.deregister();delete globalThis.__secretaryRegistryMocks;await unlink(join(directory,'config.json'));await rmdir(directory);}
