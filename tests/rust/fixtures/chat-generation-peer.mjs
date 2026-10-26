// Synthetic JSON-RPC replay peer; the subject is the native generation owner.
import './chat-store-loader.mjs';
import {readFileSync} from 'node:fs';
import {once} from 'node:events';
import {WebSocketServer} from 'ws';
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {canonical}=await import('../../../packages/contracts/src/canonical.ts');
const {toolDefinitionHash}=await import('../../../packages/contracts/src/tool-definition.ts');
const replay=JSON.parse(readFileSync(process.argv[3],'utf8'));
let generation=6;
const wss=new WebSocketServer({host:'127.0.0.1',port:0,perMessageDeflate:false,maxPayload:32*1024*1024});
await once(wss,'listening');
const base=`http://127.0.0.1:${wss.address().port}/ivy/`;
let invokeId=0;
wss.on('connection',socket=>{
 let spec,pending=new Map(),index=0,selectedGeneration=++generation,mismatches=[];
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

   send({jsonrpc:'2.0',id:key,method:'provider.invoke',params:{qualifiedName:'chat.workspace',arguments:{},definitionHash:toolDefinitionHash(definition),generation:selectedGeneration,callerPrincipalId:'alice'}});return;
  }
  try{

    const expected=spec.calls[index++];
    if(!expected||method!==expected.method||canonical(params)!==canonical(expected.params)){
     const mismatch={method,params,expected};mismatches.push(mismatch);
     throw new IvyError('fixture_rpc_mismatch','Generation RPC differs: '+JSON.stringify(mismatch));
    }
    if(expected.error)throw new IvyError(expected.error.code,expected.error.message,expected.error.outcome);
    reply(id,expected.value);return;
  }catch(e){send({jsonrpc:'2.0',id,error:IvyError.from(e).toWire()});}
 });
});
console.log(JSON.stringify({url:base}));
