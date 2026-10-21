// Execute unchanged common HTTP spans from the original owner. Console cases
// enter the final route after authentication; this harness grants no credentials.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {readFile,realpath,stat} from 'node:fs/promises';
import {join,resolve,sep} from 'node:path';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
 if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const path=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(path)))return next(path.href,context);}
 return next(specifier,context);
},load(url,context,next){
 if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
 return next(url,context);
}});
const {IvyError,requireThat,fail}=await import('../../packages/contracts/src/errors.ts');
const {connectionInFlightRequests}=await import('../../packages/contracts/src/limits.ts');
const {RequestBudget}=await import('../../services/hive/src/admission.ts');
const {uiContentPolicy}=await import('../../services/hive/src/ui-security.ts');
const {browserAppManifest,browserAppWorker}=await import('../../services/hive/src/browser-app.ts');
const {mcpIconSvg}=await import('../../services/hive/src/mcp-branding.ts');
const {assetPath}=await import('../../services/hive/src/uis.ts');
const server=readFileSync(new URL('../../services/hive/src/server.ts',import.meta.url),'utf8');
const kernel=readFileSync(new URL('../../services/hive/src/kernel.ts',import.meta.url),'utf8');
function span(text,start,end){const at=text.indexOf(start),stop=text.indexOf(end,at+start.length);if(at<0||stop<0)throw Error('Original HTTP boundaries changed: '+start);return text.slice(at,stop);}
const rpcSource=span(kernel,'export function rpcFailure(', '\nexport function validRequestId(');
const rpcFailure=new Function('IvyError',stripTypeScriptTypes(rpcSource.replace('export ',''),{mode:'transform'})+';return rpcFailure;')(IvyError);
const common=span(server,'    this.headers(response);\n    const active = this.httpActive', '      if (path === "/api/v1/dashboards/image.png")');
const admission=common.slice(0,common.indexOf('      const url = new URL('));
const publicRoutes=span(server,'      if (request.method === "GET" && path === "/app/manifest.webmanifest")', '      if (\n        request.method === "GET" &&\n        (path === "/.well-known/oauth-protected-resource"');
const finalRoute=span(server,'      if (\n        request.method === "GET" &&\n        (path === "/" || path.startsWith("/console/"))', '      fail("not_found", "Hive path or method not found.");');
const tail=span(server,'    } catch (error) {\n      if (\n        (routePath === "/mcp"', '\n  }\n  async invoke(');
const catchBody=tail.slice(tail.indexOf('      if ('),tail.indexOf('    } finally {'));
const methods=span(server,'  private headers(', '  private sameOrigin(')+span(server,'  private parse(', '  private returnPath(');
const factory=new Function('IvyError','requireThat','fail','rpcFailure','RequestBudget','connectionInFlightRequests','uiContentPolicy','browserAppManifest','browserAppWorker','mcpIconSvg','readFile','realpath','stat','join','resolve','sep','assetPath','MAX_FRAME',stripTypeScriptTypes(`class Original {
${methods}
async route(request,response){${common}return {href:url.href,pathname:url.pathname,authorizationMetadata:false,path};${tail}}
async assets(request,response){${common}${publicRoutes}${finalRoute}return false;${tail}}
async admission(request,response){${admission}await this.pause(request);${tail}}
failure(error,routePath,rpcId,response){${catchBody}}
}`,{mode:'transform'})+';return Original;');
const Original=factory(IvyError,requireThat,fail,rpcFailure,RequestBudget,connectionInFlightRequests,uiContentPolicy,browserAppManifest,browserAppWorker,mcpIconSvg,readFile,realpath,stat,join,resolve,sep,assetPath,32*1024*1024);
function response(input={}){
 const events=[];return{events,writableEnded:!!input.ended,destroyed:!!input.destroyed,headersSent:!!input.sent,
 setHeader(name,value){events.push({kind:'set',name,value});if(input.throwHeader===name)throw Error('isolated header failure');},
 writeHead(status,headers){events.push({kind:'head',status,headers});if(input.throwHead)throw Error('isolated head failure');this.headersSent=true;},
 end(value){events.push(value===undefined?{kind:'end'}:{kind:'bytes',base64:Buffer.from(value).toString('base64')});this.writableEnded=true;},
 destroy(){events.push({kind:'destroy'});this.destroyed=true;}};
}
function owner(input){const value=new Original();value.base=new URL(input.origin??'https://example.test');value.basePath=input.basePath??'/ivy';value.options={consoleRoot:input.root??undefined};value.httpActive=new Map();value.bodies=new Map();value.requests=new RequestBudget(...(input.budget??[]));value.clientAddress=request=>{if(request.addressError)throw new IvyError('unauthenticated','Isolated address failure.');return request.client??'client';};value.oauth={authorizationServerMetadata(){return {authorizationMetadata:true};}};return value;}
const error=input=>input?.code?new IvyError(input.code,input.message??'Isolated HTTP failure.',input.outcome??'not_executed',input.details):new Error('opaque isolated failure');
const rows=[];
for(const input of JSON.parse(readFileSync(0,'utf8'))){
 const value=owner(input),res=response(input.response),request={method:input.method,url:input.url,socket:{}};
 try{
  if(input.action==='parse')rows.push({value:value.parse(Buffer.from(input.base64,'base64'))});
  else if(input.action==='error'){value.failure(error(input.failure),input.path,input.id??null,res);rows.push({events:res.events});}
  else if(input.action==='manifest')rows.push({encoded:JSON.stringify(browserAppManifest(input.basePath))});
  else if(input.action==='admission'){
   const sockets=new Map(),running=new Map(),gates=new Map(),responses=new Map(),snapshots=[];
   value.pause=request=>new Promise(yes=>gates.set(request.id,yes));
   for(const step of input.steps){
    if(step.op==='start'){
     if(!sockets.has(step.socket))sockets.set(step.socket,{});
     const res=response(step.response),request={...step,socket:sockets.get(step.socket)};responses.set(step.id,res);
     running.set(step.id,value.admission(request,res).catch(e=>res.events.push({kind:'throw',error:IvyError.from(e).toWire()})));
    }else if(step.op==='finish'){gates.get(step.id)();await running.get(step.id);}
    else if(step.op==='grow'){try{const request=[...value.bodies.keys()].find(r=>r.id===step.id);value.bodies.get(request).grow(step.bytes);}catch(e){responses.get(step.id).events.push({kind:'growError',error:IvyError.from(e).toWire()});}}
    else if(step.op!=='snapshot')throw Error('Unknown admission command');
    await new Promise(yes=>setImmediate(yes));
    if(step.op==='snapshot')snapshots.push({name:step.name,active:[...sockets].map(([id,socket])=>[id,value.httpActive.get(socket)??0]),bodies:[...value.bodies.keys()].map(r=>r.id).sort(),events:Object.fromEntries([...responses].map(([id,res])=>[id,res.events.slice()]))});
   }
   if(value.bodies.size)throw Error('Fixture left pending HTTP requests');await Promise.all(running.values());rows.push({snapshots});
  }else{
   const result=await value[input.action==='assets'?'assets':'route'](request,res);
   rows.push(input.action==='assets'?{handled:result!==false,events:res.events}:{value:result??null,events:res.events});
  }
 }catch(e){rows.push({error:IvyError.from(e).toWire(),events:res.events});}
}
process.stdout.write(JSON.stringify(rows));
