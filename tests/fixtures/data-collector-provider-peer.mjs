import { createRequire, registerHooks } from 'node:module';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { once } from 'node:events';
const require = createRequire(process.env.IVY_TEST_PACKAGE_JSON || import.meta.url);
const { WebSocketServer } = require('ws');
registerHooks({resolve(specifier,context,next) {
  if (specifier.startsWith('.') && specifier.endsWith('.js') && context.parentURL) {
    const source = new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if (existsSync(fileURLToPath(source))) return next(source.href,context);
  }
  return next(specifier,context);
}});
const {toolDefinitionHash} = await import('../../packages/contracts/src/tool-definition.ts');
const server = new WebSocketServer({host:'127.0.0.1',port:0,perMessageDeflate:false});
await once(server,'listening');
let generation=0,invocation=0;
server.on('connection',socket=>{
  const current=++generation,trace=[],pending=new Map();let registry;
  const reply=(id,result)=>socket.send(JSON.stringify({jsonrpc:'2.0',id,result}));
  socket.on('message',bytes=>{
    const frame=JSON.parse(bytes.toString());
    if (!frame.method) { const id=pending.get(frame.id);pending.delete(frame.id);if(id)reply(id,frame);return; }
    const {method,params,id}=frame;trace.push(method);
    if(method==='service.connect')reply(id,{generation:current});
    else if(method==='registry.sync'){registry=params;reply(id,{});}
    else if(method==='service.heartbeat')reply(id,{ready:params.ready});
    else if(method==='test.info')reply(id,{trace,registry,generation:current});
    else if(method==='test.reconcile')reply(id,{});
    else if(method==='test.close'){reply(id,{});socket.close();}
    else if(method==='test.invoke'){
      const key='invocation-'+(++invocation),definition=registry.namespaces[0].tools.find(t=>t.name===params.tool);
      pending.set(key,id);
      socket.send(JSON.stringify({jsonrpc:'2.0',id:key,method:'provider.invoke',params:{qualifiedName:'data-collector.'+params.tool,arguments:params.input,definitionHash:toolDefinitionHash(definition),generation:current,callerPrincipalId:'caller-\ud800',operationId:params.operationId??'operation-\udfff'}}));
    }
  });
});
console.log(JSON.stringify({url:`http://127.0.0.1:${server.address().port}/ivy/`}));
