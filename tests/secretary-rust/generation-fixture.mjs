import {WebSocketServer} from 'ws';
import {load,settings as base,owner,rootId,at} from './inbox-fixtures.mjs';
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {defaultConfiguration}=await load('services/secretary/src/assignment-schema.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {canonical,hashJson}=await load('packages/sdk/src/node.js');
const reconcile=process.argv.includes('--reconcile');
const settings={...base,...(reconcile?{sources:[]}:{}),pollMs:60000,identity:{...base.identity,serviceNodeId:'fixture.secretary'}};
const id='00000000-0000-4000-8000-000000000099';
const value=defaultConfiguration(settings,at,{serviceNodeId:'fixture.agent',threadCwd:'/workspace',model:null,effort:'medium',permissions:':read-only'});
const store=new SecretaryStore({},rootId);
let document;
try{
 const write=store.objectWriteRequest('secretary/configuration',value,{object:{objectId:id,revision:1}},'fixture:1800000000000:configuration');
 document={object:{id,parentId:rootId,effectivelyArchived:false,contractKey:'secretary/configuration'},revision:{revision:1,contractVersion:contractVersion('secretary/configuration'),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:write.references,createdAt:at},content:{encoding:'json',value}};
}finally{store.close();}
// This socket supplies only the real SDK connection lifetime. Domain RPCs are
// independently gated by the Rust test; there is no live Hive or provider.
const server=new WebSocketServer({host:'127.0.0.1',port:0});
await new Promise((resolve,reject)=>{server.once('listening',resolve);server.once('error',reject);});
if(reconcile){
 const failures=Number(process.argv[process.argv.indexOf('--failures')+1]??0)||0;
 const gated=process.argv.includes('--gate-stat');
 let remaining=failures,held=null,released=!gated;
 const statWaiters=[];
 const trace=[];
 const send=(socket,id,result,error)=>{if(socket.readyState===1)socket.send(JSON.stringify({jsonrpc:'2.0',id,...(error?{error:{code:-32000,message:error.message,data:{code:error.code,outcome:error.outcome}}}:{result})}));};
 server.on('connection',socket=>socket.on('message',bytes=>{
  const {id,method,params}=JSON.parse(bytes.toString());
  if(method==='fixture.trace'){send(socket,id,trace);return;}
  if(method==='fixture.waitStat'){
   if(trace.some(row=>row.method==='objects.stat'))send(socket,id,{});else statWaiters.push({socket,id});return;
  }
  if(method==='fixture.release'){released=true;if(held){send(held.socket,held.id,{effectivelyArchived:false});held=null;}send(socket,id,{});return;}
  trace.push({method,params});
  if(method==='system.status'){send(socket,id,{runtimeEpoch:'fixture-epoch'});return;}
  if(method==='serviceNodes.get'){
   if(remaining-->0){send(socket,id,null,{code:'limit_exceeded',message:'Fixture initialization limit.',outcome:'unknown'});return;}
   send(socket,id,owner);return;
  }
  if(method==='objects.stat'){
   for(const waiter of statWaiters.splice(0))send(waiter.socket,waiter.id,{});
   if(!released){held={socket,id};return;}
   send(socket,id,{effectivelyArchived:false});return;
  }
  if(method==='objects.query'){
   send(socket,id,{items:params.contractKey==='secretary/configuration'?[{objectId:document.object.id,revision:1}]:[],nextCursor:null});return;
  }
  if(method==='objects.read'){send(socket,id,document);return;}
  send(socket,id,null,{code:'fixture_unexpected',message:'Unexpected lifecycle fixture RPC '+method,outcome:'not_executed'});
 }));
}
process.stdout.write(JSON.stringify({port:server.address().port,settings,owner,rootId,document})+'\n');
process.stdin.resume();
process.stdin.on('end',()=>{for(const socket of server.clients)socket.terminate();server.close();});
