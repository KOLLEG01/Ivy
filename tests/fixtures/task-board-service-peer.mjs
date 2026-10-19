import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {once} from 'node:events';
import {WebSocketServer} from 'ws';

registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
}});
const {toolDefinitionHash}=await import('../../packages/contracts/src/tool-definition.ts');
// An explicit isolated catalog, not the accepted TaskBoard registry/contract overlay.
const definition={namespace:'task-board',name:'workspace',description:'Isolated owner invocation.',interfaceVersion:'1.0.0',inputSchema:{},outputSchema:{}};
const registry={namespaces:[{namespace:'task-board',description:'Fixture',guideMarkdown:'',tools:[definition],topics:[],inventoryKinds:[]}],contracts:[],requiredContracts:[]};
const wss=new WebSocketServer({host:'127.0.0.1',port:0,perMessageDeflate:false,maxPayload:32*1024*1024});
await once(wss,'listening');
console.log(JSON.stringify({url:`http://127.0.0.1:${wss.address().port}/ivy/`,registry}));
let generation=0,invocation=0;
wss.on('connection',(socket,request)=>{
  const selected=++generation,pending=new Map();
  const send=frame=>{if(socket.readyState===1)socket.send(JSON.stringify(frame));};
  const reply=(id,result)=>send({jsonrpc:'2.0',id,result});
  const emit=value=>console.log(JSON.stringify({generation:selected,...value}));
  emit({kind:'open',authorization:request.headers.authorization,origin:request.headers.origin,path:request.url,extensions:request.headers['sec-websocket-extensions']??null});
  socket.on('message',bytes=>{
    const frame=JSON.parse(bytes.toString());
    if(!frame.method){
      emit({kind:'result',frame});
      const id=pending.get(frame.id);pending.delete(frame.id);
      if(id!==undefined)reply(id,frame);
      return;
    }
    const {method,params,id}=frame;emit({kind:'rpc',method,params});
    if(method==='service.connect')reply(id,{generation:selected});
    else if(method==='service.heartbeat')reply(id,{ready:params.ready});
    else if(['registry.sync','notifications.subscribe','objects.stat'].includes(method))reply(id,{});
    else if(method==='serviceNodes.get')reply(id,{serviceNodeId:params.serviceNodeId,connected:true,synced:true,principalId:'fixture-principal'});
    else if(method==='test.invoke'){
      const key='invoke-'+(++invocation);pending.set(key,id);
      send({jsonrpc:'2.0',id:key,method:'provider.invoke',params:{qualifiedName:'task-board.workspace',arguments:params,definitionHash:toolDefinitionHash(definition),generation:selected,callerPrincipalId:'caller-\ud800',operationId:'operation-\udfff'}});
    }else if(method==='test.notification'){
      send({jsonrpc:'2.0',method:'notifications.provider',params:{namespace:'agent',name:'notification',version:'1.0.0',payload:params,serviceNodeId:'fixture-agent',generation:selected}});
      reply(id,{});
    }else if(method==='test.close'){reply(id,{});socket.close();}
    else send({jsonrpc:'2.0',id,error:{code:-32601,message:'Unknown fixture method.'}});
  });
  socket.on('close',()=>emit({kind:'closed'}));
});
