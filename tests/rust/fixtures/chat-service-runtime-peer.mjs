// Synthetic public-protocol peer; it does not load a service implementation.
import './chat-store-loader.mjs';
import {readFileSync} from 'node:fs';
import {gunzipSync} from 'node:zlib';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {WebSocketServer} from 'ws';
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {toolDefinitionHash}=await import('../../../packages/contracts/src/tool-definition.ts');
const {canonical}=await import('../../../packages/contracts/src/canonical.ts');
const createdSeeds=JSON.parse(gunzipSync(readFileSync(new URL('chat-created-main-seeds.json.gz',import.meta.url))));
const error=e=>{e=IvyError.from(e);return{code:e.code,message:e.message,outcome:e.outcome};};
let selected,sockets=new Map(),pending=new Map(),requestId=0;
const wss=new WebSocketServer({host:'127.0.0.1',port:0,perMessageDeflate:false,maxPayload:32*1024*1024});
await once(wss,'listening');
const base=`http://127.0.0.1:${wss.address().port}/ivy/`;
const send=(socket,value)=>{if(socket.readyState===socket.OPEN)socket.send(JSON.stringify(value));};
function begin(spec){selected={spec,generation:6,calls:[],heartbeats:[],handshakes:[],mismatches:[]};return selected;}
function summary(){return{calls:selected.calls,heartbeats:selected.heartbeats,handshakes:selected.handshakes,mismatches:selected.mismatches};}
async function command(c){
 if(c.action==='begin'){begin(c.spec);return{url:base};}
 if(c.action==='summary')return summary();
 if(c.action==='mediaSeed')return structuredClone(createdSeeds[c.version]);
 if(c.action==='disconnect'){for(const socket of sockets.keys())socket.close();return null;}
 if(c.action==='invoke'){
  const [socket,session]=[...sockets].find(([,s])=>s.generation===c.generation)??[];
  if(!socket||!session.registry)throw Error('No registered provider generation');
  const id='runtime-invoke-'+(++requestId),name=c.name??'workspace',definition=session.registry.namespaces[0].tools.find(t=>t.name===name);
  const response=new Promise(resolve=>pending.set(id,resolve));
  send(socket,{jsonrpc:'2.0',id,method:'provider.invoke',params:{qualifiedName:'chat.'+name,arguments:c.arguments??{},definitionHash:toolDefinitionHash(definition),generation:session.generation,callerPrincipalId:'alice'}});
  return response;
 }
 throw Error('Unknown fixture command '+c.action);
}
wss.on('connection',socket=>{
 const scope=selected,session={generation:++scope.generation,registry:null};sockets.set(socket,session);
 socket.on('close',()=>sockets.delete(socket));
 socket.on('message',async bytes=>{
  const frame=JSON.parse(bytes.toString());
  if(!frame.method){pending.get(frame.id)?.(frame);pending.delete(frame.id);return;}
  const {id,method,params}=frame;
  const call={method,params};scope.calls.push(call);
  try{
   let value;
   if(method==='service.connect'){scope.handshakes.push({generation:session.generation,method,params});value={generation:session.generation};}
   else if(method==='registry.sync'){session.registry=params;scope.handshakes.push({generation:session.generation,method,params});value={};}
   else if(method==='notifications.subscribe'){scope.handshakes.push({generation:session.generation,method,params});value={};}
   else if(method==='service.heartbeat'){scope.heartbeats.push({generation:session.generation,...params});value={ready:params.ready};}
   else if(method==='serviceNodes.get')value={principalId:'chat-service',serviceName:'chat-bridge',connected:true,synced:true};
   else if(method==='system.status')value={runtimeEpoch:'chat'};
   else if(method==='objects.write'&&scope.spec.remoteMedia){
    const prior=scope.imageWrite;
    if(prior&&canonical(prior)!==canonical(params))throw new IvyError('fixture_image_changed','Retried immutable image request changed.');
    scope.imageWrite??=structuredClone(params);
    value={object:{id:'remote-image'},revision:{revision:1}};
    if(!prior)throw new IvyError('fixture_image_reply_lost','Synthetic committed image reply was lost.','unknown');
   }
   else if(method==='system.diagnostics'){
    if(scope.spec.label==='diagnostic-read-failure')throw new IvyError('fixture_diagnostic_unavailable','Original diagnostic source is unavailable.','unknown');
    value={items:[],nextCursor:null};
   }else if(method==='settings.get')value={settings:{quietHours:null}};
   else if(method==='tools.list')throw new IvyError('fixture_native_offline','Native owner is intentionally offline.','unknown');
   else{const mismatch={method,params};scope.mismatches.push(mismatch);throw new IvyError('fixture_rpc_mismatch','Unexpected service RPC '+method);}
   call.value=value;send(socket,{jsonrpc:'2.0',id,result:value});
  }catch(e){call.error=error(e);send(socket,{jsonrpc:'2.0',id,error:IvyError.from(e).toWire()});}
 });
});

console.log(JSON.stringify({url:base}));
for await(const line of createInterface({input:process.stdin,crlfDelay:Infinity})){
 try{console.log(JSON.stringify({value:await command(JSON.parse(line))}));}
 catch(e){console.log(JSON.stringify({error:error(e)}));}
}
