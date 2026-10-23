// An isolated Hive protocol peer; never starts a Phone/Voice implementation.
import {once} from 'node:events';
import {createServer} from 'node:http';
import {WebSocketServer} from 'ws';
let active=null;
const info=()=>({identity:active?.identity??null, heartbeat:active?.heartbeat??null,
  tools:active?.registry?.namespaces.flatMap(ns=>ns.tools.map(t=>`${ns.namespace}.${t.name}`))??[]});
const http=createServer(async (request,response)=>{
  const send=(status,value)=>{response.writeHead(status,{'content-type':'application/json'});response.end(JSON.stringify(value));};
  if(request.method==='GET'&&request.url==='/ivy/fixture'){send(200,info());return;}
  if(request.method!=='POST'||request.url!=='/ivy/api/v1/rpc'){send(404,{});return;}
  if(request.headers.authorization!=='Bearer isolated-fixture'){send(401,{});return;}
  try{
    let body='';for await(const chunk of request){body+=chunk;if(body.length>65536)throw Error('Fixture frame too large');}
    const frame=JSON.parse(body);
    if(frame.method!=='serviceNodes.get'||frame.params.serviceNodeId!==active?.identity?.serviceNodeId)throw Error('Unexpected fixture RPC');
    send(200,{jsonrpc:'2.0',id:frame.id,result:{connected:true,synced:!!active.registry,
      ready:active.heartbeat?.ready===true,buildId:active.identity.buildId}});
  }catch{send(400,{});}
});
const server = new WebSocketServer({server:http,perMessageDeflate:false});
http.listen(0,'127.0.0.1');await once(http,'listening');
let generation=0, sequence=0;
server.on('connection',socket=>{
  const current=++generation, pending=new Map();
  const state={identity:null,registry:null,heartbeat:null};active=state;
  socket.on('close',()=>{if(active===state)active=null;});
  const send=frame=>socket.send(JSON.stringify(frame));
  const reply=(id,result)=>send({jsonrpc:'2.0',id,result});
  socket.on('message',bytes=>{
    const frame=JSON.parse(bytes.toString()), {method,params,id}=frame;
    if(!method){const resolve=pending.get(id);pending.delete(id);resolve?.(frame);return;}
    if(method==='service.connect'){state.identity=params;reply(id,{generation:current});}
    else if(method==='registry.sync'){state.registry=params;reply(id,{});}
    else if(method==='service.heartbeat'){state.heartbeat=params;reply(id,{ready:params.ready});}
    else if(method==='fixture.info')reply(id,{generation:current,...info()});
    else if(method==='fixture.invoke'){
      const invocation=`fixture-${++sequence}`;
      pending.set(invocation,result=>reply(id,result));
      send({jsonrpc:'2.0',id:invocation,method:'provider.invoke',params:{
        qualifiedName:params.qualifiedName,arguments:params.arguments,definitionHash:params.definitionHash,
        generation:params.generation??current,callerPrincipalId:'fixture-reader',
      }});
    } else if(method==='fixture.disconnect'){reply(id,{});setImmediate(()=>socket.close());}
    else throw new Error(`Unexpected isolated protocol method ${method}`);
  });
});
console.log(`http://127.0.0.1:${http.address().port}/ivy/`);
process.stdin.resume();
process.stdin.on('end',()=>{for(const socket of server.clients)socket.terminate();server.close();http.close();});
