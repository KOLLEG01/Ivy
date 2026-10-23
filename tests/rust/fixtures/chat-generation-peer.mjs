// Exercise the unchanged main.ts reconciliation callback and real SDK sockets.
// Host file I/O and autonomous timers are outside this generation-owner fixture.
import {starting,now,instructions,noticeInstructions} from './chat-main-oracle.mjs';
import {registerHooks} from 'node:module';
import {mkdtempSync,writeFileSync,readFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {once} from 'node:events';
import {setTimeout as delay} from 'node:timers/promises';
import {WebSocketServer} from 'ws';
const url=p=>new URL(p,import.meta.url).href;
const data=s=>'data:text/javascript;base64,'+Buffer.from(s).toString('base64');
const probe=Symbol.for('ivy.test.chat-generation');
const sdk=data(`export{IvyError,requireThat}from'${url('../../../packages/contracts/src/errors.ts')}';
export{validateChat}from'${url('../../../packages/contracts/src/chat-validation.ts')}';
import{ServiceClient as Real}from'${url('../../../packages/sdk/src/service.ts')}';
export class ServiceClient extends Real{start(){} async stop(){for(const c of globalThis[Symbol.for('ivy.test.chat-generation')].connections)c.close();} get ready(){return false;}}`);
const host=data(`export async function instanceConfig(){return globalThis[Symbol.for('ivy.test.chat-generation')].config;}
export function configurationPath(){throw Error('Outside generation fixture');}
export class HealthFile{async write(){} async control(){return null;}}`);
const registrySdk=data(`export{bundledSchema}from'${url('../../../packages/contracts/src/bundle.ts')}';export{chatInterfaceVersion}from'${url('../../../packages/sdk/src/main-chat.ts')}';`);
const diagnosticsSdk=data(`export{automaticNotificationsAllowed}from'${url('../../../packages/sdk/src/contact-window.ts')}';export{hashJson}from'${url('../../../packages/contracts/src/canonical.ts')}';export{newOperationId}from'${url('../../../packages/sdk/src/client.ts')}';export{requireThat}from'${url('../../../packages/contracts/src/errors.ts')}';`);
registerHooks({resolve(s,c,n){
 if(c.parentURL?.endsWith('/services/chat-bridge/src/main.ts')){
  if(s==='../../../packages/sdk/src/node.js')return{url:sdk,shortCircuit:true};
  if(s==='../../../packages/sdk/src/host.js')return{url:host,shortCircuit:true};
  if(s==='./whatsapp/runtime.js')return{url:data('export class WhatsAppRuntime{static open(){throw Error("No account transport in generation fixture");}}'),shortCircuit:true};
 }
 if(c.parentURL?.endsWith('/services/chat-bridge/src/registry.ts')&&s==='../../../packages/sdk/src/node.js')return{url:registrySdk,shortCircuit:true};
 if(c.parentURL?.endsWith('/services/chat-bridge/src/diagnostic-notifications.ts')&&s==='../../../packages/sdk/src/node.js')return{url:diagnosticsSdk,shortCircuit:true};
 return n(s,c);
}});
const {startChatBridge}=await import('../../../services/chat-bridge/src/main.ts');
const {ServiceConnection}=await import('../../../packages/sdk/src/service.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {canonical}=await import('../../../packages/contracts/src/canonical.ts');
const error=e=>{e=IvyError.from(e);return{code:e.code,message:e.message,outcome:e.outcome};};
const result=async f=>{try{return{value:await f()??null};}catch(e){return{error:error(e)};}};
const cases=[];
function add(label,patch={}){cases.push({label,version:'0.154.0',node:{principalId:'chat-service',serviceName:'chat-bridge',connected:true,synced:true},...patch});}
for(const version of Object.keys(starting).filter(v=>/^0\./.test(v)))add('catalog-'+version,{version,invoke:true});
add('active-root',{root:'fixture-root',invoke:true});
add('archived-root',{root:'fixture-root',archived:true});
for(const [field,value]of Object.entries({principalId:'foreign',serviceName:'foreign',connected:false,synced:false}))add('owner-'+field,{node:{principalId:'chat-service',serviceName:'chat-bridge',connected:true,synced:true,[field]:value}});
add('status-outage',{failure:'system.status'});
add('root-outage',{root:'fixture-root',failure:'objects.stat'});
add('wrong-plan-source',{badSource:true});
add('wrong-plan-definition',{badDefinition:true});
add('preclosed-generation',{preclosed:true});
add('retirement-keeps-accepted-leases',{retire:true,invoke:true});

const corpus={now,instructions,noticeInstructions,cases,boundary:'Unchanged main.ts reconcile and its real SDK ServiceConnection execute; only Host config/health and autonomous start are intercepted. Native outage is synthetic. Retirement holds a completed invocation before the original handler settles. No Host-file, service scheduler, WhatsApp, installed or platform acceptance.'};
const replay=process.argv[2]==='peer'?JSON.parse(readFileSync(process.argv[3],'utf8')):null;
let current=null,generation=6;
const wss=new WebSocketServer({host:'127.0.0.1',port:0,perMessageDeflate:false,maxPayload:32*1024*1024});
await once(wss,'listening');
const base=`http://127.0.0.1:${wss.address().port}/ivy/`;
let invokeId=0;
wss.on('connection',socket=>{
 let spec=current,pending=new Map(),index=0,selectedGeneration=++generation,mismatches=[];
 const send=v=>{if(socket.readyState===socket.OPEN)socket.send(JSON.stringify(v));};
 const reply=(id,value)=>send({jsonrpc:'2.0',id,result:value});
 socket.on('message',async bytes=>{
  const frame=JSON.parse(bytes.toString());
  if(!frame.method){pending.get(frame.id)?.(frame);pending.delete(frame.id);return;}
  const {id,method,params}=frame;
  if(method==='fixture.case'){spec=replay.cases[params.index];selectedGeneration=params.generation;index=params.after??0;mismatches=[];reply(id,{});return;}
  if(method==='fixture.remaining'){reply(id,{remaining:(spec.calls?.length??0)-index,mismatches});return;}
  if(method==='fixture.invoke'){
   const key='generation-invoke-'+(++invokeId);pending.set(key,value=>reply(id,value));
   const definition=spec.registry.namespaces[0].tools.find(t=>t.name==='workspace');
   const {toolDefinitionHash}=await import('../../../packages/contracts/src/tool-definition.ts');
   send({jsonrpc:'2.0',id:key,method:'provider.invoke',params:{qualifiedName:'chat.workspace',arguments:{},definitionHash:toolDefinitionHash(definition),generation:selectedGeneration,callerPrincipalId:'alice'}});return;
  }
  try{
   if(replay){
    const expected=spec.calls[index++];
    if(!expected||method!==expected.method||canonical(params)!==canonical(expected.params)){
     const mismatch={method,params,expected};mismatches.push(mismatch);
     throw new IvyError('fixture_rpc_mismatch','Generation RPC differs: '+JSON.stringify(mismatch));
    }
    if(expected.error)throw new IvyError(expected.error.code,expected.error.message,expected.error.outcome);
    reply(id,expected.value);return;
   }
   const call={method,params};spec.calls.push(call);
   try{
    if(method===spec.failure)throw new IvyError('fixture_unavailable','Original generation dependency is unavailable.','unknown');
    let value;
    if(method==='serviceNodes.get')value=spec.node;
    else if(method==='system.status')value={runtimeEpoch:'chat'};
    else if(method==='objects.stat')value={id:params.objectId,effectivelyArchived:!!spec.archived};
    else if(method==='tools.list')throw new IvyError('fixture_native_offline','Native owner is intentionally offline.','unknown');
    else throw Error('Unexpected generation RPC '+method);
    call.value=value;reply(id,value);
   }catch(e){call.error=error(e);throw e;}
  }catch(e){send({jsonrpc:'2.0',id,error:IvyError.from(e).toWire()});}
 });
});
if(replay){console.log(JSON.stringify({url:base}));}
else{
 const originalInvoke=(await import('../../../services/chat-bridge/src/bridge.ts')).ChatBridge.prototype.invoke;
 const bridgeClass=(await import('../../../services/chat-bridge/src/bridge.ts')).ChatBridge;
 let gate=null;
 bridgeClass.prototype.invoke=async function(...args){const value=await originalInvoke.apply(this,args);if(gate){gate.entered=true;await gate.promise;}return value;};
 for(const spec of cases){
  const root=mkdtempSync(join(tmpdir(),'ivy-chat-generation-original-'));
  const seed=starting[spec.version];spec.definition=structuredClone(seed.definition);
  if(spec.root)spec.definition.rootObjectId=spec.root;
  if(spec.badSource)spec.definition.nativePlan.catalogSourceHash='sha256:'+'0'.repeat(64);
  if(spec.badDefinition)spec.definition.nativePlan.definitions.threadStart='sha256:'+'0'.repeat(64);
  spec.database=seed.database;writeFileSync(join(root,'chat-bridge.sqlite'),Buffer.from(seed.database,'base64'));
  const config={componentId:'chat-bridge',credential:'fixture-token',dataRoot:root,serviceNodeId:'chat-provider',hostId:'fixture-host',version:'1.0.0',buildId:'sha256:'+'a'.repeat(64),publicBaseUrl:base,settings:{pollMs:60000,definition:spec.definition}};
  globalThis[probe]={config,connections:[]};spec.calls=[];current=spec;
  const app=await startChatBridge('fixture-config');
  const registry=app.service.options.registry();spec.registry=registry;
  const connect=async()=>{
   const socketUrl=new URL('ws',base);socketUrl.protocol='ws:';
   const c=new ServiceConnection(socketUrl,'fixture-token',app.service);await c.opened;
   c.generation=generation;c.setRegistry(registry);c.ready=true;
   globalThis[probe].connections.push(c);return c;
  };
  let c=await connect();spec.generation=c.generation;
  if(spec.preclosed)c.close();
  spec.reconciled=await result(()=>app.service.options.reconcile(c));
  if(spec.reconciled.value===null&&spec.invoke){
   if(spec.retire){let release;gate={entered:false,promise:new Promise(done=>release=done)};
    const pending=c.request('fixture.invoke',{}).catch(()=>undefined);
    while(!gate.entered)await delay(1);
    const old=app.runtime;c.close();
    spec.readableWhileAccepted=await result(()=>old.main.store.named('chat-bridge/main','retiring-generation'));
    c=await connect();spec.reopened=await result(()=>app.service.options.reconcile(c));
    release();gate=null;await pending;
    for(let n=0;n<100;n++){const state=await result(()=>old.main.store.named('chat-bridge/main','retiring-generation'));if(state.error){spec.closedAfterAccepted=true;break;}await delay(1);}
    if(!spec.closedAfterAccepted)throw Error('Original retired store did not close');
   }else spec.invoked=await c.request('fixture.invoke',{});
  }
  await app.close();
  spec.retainedDatabase=readFileSync(join(root,'chat-bridge.sqlite')).toString('base64');
  rmSync(root,{recursive:true,force:true});
 }
 for(const s of cases)if((s.invoke&&!s.retire)&&!s.invoked?.result)throw Error('Original successful generation failed '+s.label+JSON.stringify(s));
 for(const socket of wss.clients)socket.terminate();await new Promise(resolve=>wss.close(resolve));
 process.stdout.write(JSON.stringify(corpus));
}
