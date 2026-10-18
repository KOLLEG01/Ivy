import { createRequire, registerHooks } from 'node:module';
import { existsSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { once } from 'node:events';
import {createServer} from 'node:http';
const require = createRequire(process.env.IVY_TEST_PACKAGE_JSON || import.meta.url);
const { WebSocketServer } = require('ws');
const dependencies=new Map(['ajv','ajv/dist/2020.js','re2js'].map(s=>[s,pathToFileURL(require.resolve(s)).href]));
registerHooks({resolve(specifier,context,next) {
  if(dependencies.has(specifier))return next(dependencies.get(specifier),context);
  if (specifier.startsWith('.') && specifier.endsWith('.js') && context.parentURL) {
    const source = new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if (existsSync(fileURLToPath(source))) return next(source.href,context);
  }
  return next(specifier,context);
}});
const {toolDefinitionHash} = await import('../../packages/contracts/src/tool-definition.ts');
const {validateShared} = await import('../../packages/contracts/src/core-validation.ts');
let generation=0,invocation=0,registration,provider,registrationBlocked=false;
const http=createServer(async(request,response)=>{
  if(request.method!=='POST'||request.url!=='/ivy/api/v1/rpc'||request.headers.authorization!=='Bearer synthetic-only')return response.writeHead(401).end();
  const chunks=[];for await(const chunk of request)chunks.push(chunk);
  const frame=JSON.parse(Buffer.concat(chunks).toString());
  let result;
  if(frame.method==='serviceNodes.get')result={...registration,ready:registration?.ready&&!registrationBlocked};
  else if(frame.method==='test.registration'){registrationBlocked=!frame.params.ready;result={};}
  else throw Error('Unexpected fixture HTTP method: '+frame.method);
  response.writeHead(200,{'Content-Type':'application/json'}).end(JSON.stringify({jsonrpc:'2.0',id:frame.id,result}));
});
const server = new WebSocketServer({server:http,perMessageDeflate:false});
http.listen(0,'127.0.0.1');await once(http,'listening');
server.on('connection',socket=>{
  const current=++generation,trace=[],pending=new Map();let registry;
  const reply=(id,result)=>socket.send(JSON.stringify({jsonrpc:'2.0',id,result}));
  socket.on('message',bytes=>{
    const frame=JSON.parse(bytes.toString());
    if (!frame.method) { const id=pending.get(frame.id);pending.delete(frame.id);if(id)reply(id,frame);return; }
    const {method,params,id}=frame;trace.push(method);
    if(method==='service.connect'){provider=socket;registration={...params,connected:true,synced:false,ready:false};reply(id,{generation:current});}
    else if(method==='registry.sync'){registry=params;registration.synced=true;reply(id,{});}
    else if(method==='service.heartbeat'){validateShared('ServiceHeartbeat',params);registration.ready=params.ready;reply(id,{ready:params.ready});}
    else if(method==='serviceNodes.get')reply(id,{...registration,ready:registration?.ready&&!registrationBlocked});
    else if(method==='test.registration'){registrationBlocked=!params.ready;reply(id,{});}
    else if(method==='test.info')reply(id,{trace,registry,generation:current});
    else if(method==='test.reconcile')reply(id,{});
    else if(process.argv.includes('--lifecycle')&&method==='objects.query') {
      const items=params.contractKey==='data-collector/root'?[{objectId:'fixture-root'}]
        :params.contractKey==='data-collector/catalog'?[{document:{object:{id:'fixture-catalog'}}}]:[];
      reply(id,{items,nextCursor:null});
    }
    else if(process.argv.includes('--lifecycle')&&method==='objects.read')reply(id,{object:{id:params.objectId,contractKey:'data-collector/root',effectivelyArchived:false}});
    else if(method==='test.close'){reply(id,{});socket.close();}
    else if(method==='test.invoke'){
      const key='invocation-'+(++invocation),definition=registry.namespaces[0].tools.find(t=>t.name===params.tool);
      pending.set(key,id);
      socket.send(JSON.stringify({jsonrpc:'2.0',id:key,method:'provider.invoke',params:{qualifiedName:'data-collector.'+params.tool,arguments:params.input,definitionHash:toolDefinitionHash(definition),generation:current,callerPrincipalId:'caller-\ud800',operationId:params.operationId??'operation-\udfff'}}));
    }
  });
  socket.on('close',()=>{if(socket===provider)registration.connected=false;});
});
console.log(JSON.stringify({url:`http://127.0.0.1:${server.address().port}/ivy/`}));
