import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {once} from 'node:events';
import {setTimeout as delay} from 'node:timers/promises';
import {WebSocketServer} from 'ws';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
}});
const {toolDefinitionHash}=await import('../../packages/contracts/src/tool-definition.ts');
const definition={namespace:'fixture',name:'echo',description:'Echo an isolated payload.',interfaceVersion:'1.0.0',inputSchema:{},outputSchema:{}};
const identity={serviceNodeId:'fixture-node',serviceName:'fixture',hostId:'fixture-host',version:'1.0.0',buildId:'sha256:'+'a'.repeat(64),hiveProtocol:1};
const registry={namespaces:[{namespace:'fixture',description:'Fixture',guideMarkdown:'',tools:[definition],topics:[],inventoryKinds:[]}],contracts:[],requiredContracts:[]};
const wss=new WebSocketServer({host:'127.0.0.1',port:0,perMessageDeflate:false,maxPayload:32*1024*1024});
await once(wss,'listening');
const url=`http://127.0.0.1:${wss.address().port}/ivy/`;
let generation=0,invocation=0;
const all=[];
wss.on('connection',(socket,request)=>{
  const trace=[],results=[],acks=[],notifications=[],pending=new Map();let subscribed=0,heartbeats=0,ownerReads=0,ownerConnected=true;
  const record={generation:++generation,trace,results,acks,notifications,header:{origin:request.headers.origin,authorization:request.headers.authorization,path:request.url,extensions:request.headers['sec-websocket-extensions']??null}};all.push(record);
  const send=frame=>{if(socket.readyState===1)socket.send(JSON.stringify(frame));};
  const reply=(id,result)=>send({jsonrpc:'2.0',id,result});
  const invoke=(params={})=>{
    const id='invoke-'+(++invocation);
    send({jsonrpc:'2.0',id,method:'provider.invoke',params:{qualifiedName:'fixture.echo',arguments:params.arguments??{value:'fixture'},definitionHash:params.definitionHash??toolDefinitionHash(definition),generation:params.generation??record.generation,callerPrincipalId:'caller-\ud800',operationId:'operation-\udfff',...params.extra}});
    return id;
  };
  socket.on('message',bytes=>{
    const frame=JSON.parse(bytes.toString());
    if(!frame.method){results.push(frame);const resolve=pending.get(frame.id);pending.delete(frame.id);resolve?.(frame);return;}
    const {method,params,id}=frame;trace.push(method);
    if(method==='service.connect')reply(id,{generation:record.generation});
    else if(method==='registry.sync'||method==='notifications.subscribe')reply(id,{});
    else if(method==='service.heartbeat'){
      heartbeats++;
      if(request.url.startsWith('/slow/')&&heartbeats===1){invoke({arguments:{beforeAck:true}});setTimeout(()=>reply(id,{ready:params.ready}),250);}
      else reply(id,{ready:params.ready});
    }
    else if(method==='service.notification')notifications.push(params);
    else if(method==='test.echo')reply(id,params);
    else if(method==='test.ping'){socket.once('pong',bytes=>reply(id,bytes.toString()));socket.ping('fixture-ping');}
    else if(method==='test.ownerState'){ownerConnected=params.connected;reply(id,{});}
    else if(method==='serviceNodes.get')reply(id,{serviceNodeId:params.serviceNodeId,connected:ownerConnected,synced:true,ready:false,reads:++ownerReads});
    else if(method==='test.delay')setTimeout(()=>reply(id,params),params.ms);
    else if(method==='test.badResult')send({jsonrpc:'2.0',id,result:true,unexpected:true});
    else if(method==='test.info')reply(id,{...record,connections:all.length,previous:all.slice(0,-1).map(r=>({generation:r.generation,trace:r.trace,results:r.results}))});
    else if(method==='test.invoke'){
      const key=invoke(params);pending.set(key,result=>reply(id,result));
    }
    else if(method==='test.startInvocations'){
      for(let i=0;i<params.count;i++)invoke(params);
      reply(id,{});
    }
    else if(method==='test.hints'){
      for(const throughSequence of params.sequences??[])send({jsonrpc:'2.0',method:'events.available',params:{throughSequence}});
      if(params.notification)send({jsonrpc:'2.0',method:'notifications.provider',params:{namespace:'fixture',name:'notice',version:'1.0.0',payload:{text:'\ud800'},serviceNodeId:'fixture-node',generation:record.generation}});
      reply(id,{});
    }
    else if(method==='test.binary')socket.send(Buffer.from('binary'));
    else if(method==='test.malformed')socket.send('{');
    else if(method==='test.badHint')send({jsonrpc:'2.0',method:'events.available',params:{throughSequence:0}});
    else if(method==='test.close')socket.close();
    else if(method==='events.subscribe')reply(id,{name:params.name,events:[],throughSequence:++subscribed,hasMore:subscribed===1,...(subscribed===1?{gap:{prunedThroughSequence:1}}:{})});
    else if(method==='events.ack'){acks.push(params);reply(id,{});}
  });
});
if(process.argv[2]!=='oracle'){
  console.log(JSON.stringify({url,identity,registry}));
}else{
  const {ServiceClient,ServiceConnection}=await import('../../packages/sdk/src/service.ts');
  const {IvyError}=await import('../../packages/contracts/src/errors.ts');
  const notices=[];
  const owner=new ServiceClient({publicBaseUrl:url,credential:()=> 'fixture-token',identity,registry:()=>registry,handlers:{'fixture.echo':(args,context)=>({args,callerPrincipalId:context.callerPrincipalId,operationId:context.operationId,generation:context.generation})},reconcile:async()=>{},onNotification:frame=>notices.push(frame.params)});
  const socketUrl=new URL('ws',url);socketUrl.protocol='ws:';
  const c=new ServiceConnection(socketUrl,'fixture-token',owner);await c.opened;
  c.generation=(await c.request('service.connect',identity)).generation;c.setRegistry(registry);c.ready=true;
  const out=[];
  const capture=async fn=>{try{return {ok:true,value:await fn()};}catch(e){return {ok:false,error:{code:e.code,message:e.message,outcome:e.outcome}};}};
  out.push(await capture(()=>c.request('test.echo',{text:'\ud800',array:[1,null,false]})));
  out.push(await capture(()=>c.request('test.ping',{})));
  out.push(await capture(()=>c.request('test.delay',{ms:75},{timeoutMs:10})));
  await delay(90);out.push(await capture(()=>c.request('test.echo',{afterTimeout:true})));
  out.push(await capture(()=>c.request('test.badResult',{})));
  const pre=new AbortController();pre.abort(new IvyError('fixture_cancel','Before sending.'));
  out.push(await capture(()=>c.request('test.echo',{}, {signal:pre.signal})));
  const cancel=new AbortController();const waiting=c.request('test.delay',{ms:75},{signal:cancel.signal});setTimeout(()=>cancel.abort(),10);
  out.push(await capture(()=>waiting));
  await delay(90);
  for(const params of [{},{generation:2},{definitionHash:'sha256:'+'b'.repeat(64)}]){
    const value=await c.request('test.invoke',params);delete value.id;out.push(value);
  }
  await c.request('test.hints',{sequences:[4,2],notification:true});await c.waitForEvents(3,1000);out.push(notices);
  c.notification('fixture','notice','1.0.0',{text:'\udfff'});
  const info=await c.request('test.info',{});out.push({notifications:info.notifications,origin:info.header.origin===new URL(url).origin,authorization:info.header.authorization,path:info.header.path,extensions:info.header.extensions});
  out.push(await capture(()=>c.request('test.binary',{})));
  c.close();for(const socket of wss.clients)socket.terminate();await new Promise(resolve=>wss.close(resolve));
  console.log(JSON.stringify(out));
}
