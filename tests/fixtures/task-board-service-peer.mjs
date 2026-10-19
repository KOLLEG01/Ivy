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
const actionDefinition={...definition,name:'update',description:'Isolated retained action invocation.'};
const retirement=process.argv.includes('--factory-retirement');
const factory=process.argv.includes('--factory')||retirement;
const registry={namespaces:[{namespace:'task-board',description:'Fixture',guideMarkdown:'',tools:[definition,...(process.argv.includes('--engine')||factory?[actionDefinition]:[])],topics:[],inventoryKinds:[]}],contracts:[],requiredContracts:[]};
const wss=new WebSocketServer({host:'127.0.0.1',port:0,perMessageDeflate:false,maxPayload:32*1024*1024});
await once(wss,'listening');
console.log(JSON.stringify({url:`http://127.0.0.1:${wss.address().port}/ivy/`,registry}));
let generation=0,invocation=0;
let nativeInputs=[];
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
    else if(method==='objects.stat'&&selected===1&&params.objectId==='missing-root')send({jsonrpc:'2.0',id,error:{code:-32000,message:'Refused root.',data:{code:'fixture_refusal',outcome:'unknown',details:{phase:'root'}}}});
    else if(['registry.sync','notifications.subscribe','objects.stat'].includes(method))reply(id,{});
    else if(method==='serviceNodes.get')reply(id,{serviceNodeId:params.serviceNodeId,serviceName:'task-board',connected:true,synced:true,principalId:'fixture-principal'});
    else if(factory&&method==='system.status')reply(id,{runtimeEpoch:'11111111-1111-4111-8111-111111111111'});
    else if(factory&&method==='events.head')reply(id,{throughSequence:0});
    else if(factory&&method==='events.subscribe')reply(id,{items:[],throughSequence:0,gap:null});
    else if(factory&&method==='events.ack')reply(id,{});
    else if(factory&&method==='test.inputs'){nativeInputs=structuredClone(params.items);reply(id,{});}
    else if(factory&&method==='tools.list'){
      const name=params.namePrefix;
      // Retired unstarted Runs use their saved allocation when an optional
      // read-only workspace refresh is unavailable. No native call is bound.
      if(retirement){
        if(name!=='resolveWorkspace')throw Error('Unexpected retired-run tool '+name);
        send({jsonrpc:'2.0',id,error:{code:-32000,message:'Finite saved allocation boundary.',data:{code:'service_unavailable',outcome:'not_executed',details:{scope:'retained-workspace'}}}});
        return;
      }
      if(!['inputs','status'].includes(name))throw Error('Unbound finite native tool '+name);
      const nativeDefinition={namespace:'agent',name,interfaceVersion:'1.0.0',annotations:{readOnlyHint:true},inputSchema:{},outputSchema:{}};
      reply(id,{provider:{node:{serviceNodeId:'native-agent'}},items:[{qualifiedName:'agent.'+name,definition:nativeDefinition,definitionHash:toolDefinitionHash(nativeDefinition),catalogHash:'sha256:'+'b'.repeat(64)}],nextCursor:null});
    }
    else if(factory&&method==='tools.call'&&params.qualifiedName==='agent.inputs')reply(id,{epoch:'epoch',items:nativeInputs,truncated:false});
    // This explicit read-only boundary keeps native dispatch/status/evidence
    // outside the mirroring fixture; the real orchestrator records its refusal.
    else if(factory&&method==='tools.call'&&params.qualifiedName==='agent.status')send({jsonrpc:'2.0',id,error:{code:-32000,message:'Finite native inspection boundary.',data:{code:'fixture_native_stop',outcome:'not_executed',details:{scope:'native-mirroring'}}}});
    else if(method==='test.invoke'||method==='test.invokeAction'){
      const key='invoke-'+(++invocation);pending.set(key,id);
      const tool=method==='test.invoke'?definition:actionDefinition;
      send({jsonrpc:'2.0',id:key,method:'provider.invoke',params:{qualifiedName:'task-board.'+tool.name,arguments:params,definitionHash:toolDefinitionHash(tool),generation:selected,callerPrincipalId:'caller-\ud800',operationId:params.operationId??'operation-\udfff'}});
    }else if(method==='test.notification'){
      send({jsonrpc:'2.0',method:'notifications.provider',params:{namespace:'agent',name:'notification',version:'1.0.0',payload:params,serviceNodeId:'fixture-agent',generation:selected}});
      reply(id,{});
    }else if(method==='test.close'){reply(id,{});socket.close();}
    else send({jsonrpc:'2.0',id,error:{code:-32601,message:'Unknown fixture method.'}});
  });
  socket.on('close',()=>emit({kind:'closed'}));
});
