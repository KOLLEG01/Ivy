// Execute exact server method/route spans. Authentication, HTTP stream/socket and
// response I/O are explicit fixture boundaries; this is not an installed server.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath,pathToFileURL} from 'node:url';
registerHooks({resolve(specifier,context,next){
 if(specifier==='web-push'&&process.env.IVY_HIVE_TEST_WEB_PUSH)return next(pathToFileURL(process.env.IVY_HIVE_TEST_WEB_PUSH).href,context);
 if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
 return next(specifier,context);
},load(url,context,next){
 if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
 return next(url,context);
}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {RequestBudget}=await import('../../services/hive/src/admission.ts');
const source=readFileSync(new URL('../../services/hive/src/server.ts',import.meta.url),'utf8');
function span(start,end){const at=source.indexOf(start),stop=source.indexOf(end,at+start.length);if(at<0||stop<0)throw Error('Original server span unavailable');return source.slice(at,stop);}
const json=span('  private json(', '  private sameOrigin(');
const origin=span('  private sameOrigin(', '  private mcpOrigin(');
const body=span('  private async body(', '  private parse(');
const route=span('      if (path === "/api/browser-push" && request.method === "POST") {','      if (context.sessionDigest) {');
// Observe the original HTTP owner's finally before its async function settles.
// An additional await around the extracted route would add a fixture-only yield
// to the detached push coordinator before the request lease is released.
const Factory=new Function('IvyError','requireThat','MAX_FRAME',stripTypeScriptTypes(`class Original {${json}${origin}${body} async route(request,response,context,finished){let failure;try{const path='/api/browser-push';${route}}catch(error){failure=error;}finally{finished(failure);}}}`,{mode:'transform'})+'; return Original;');
const Original=Factory(IvyError,requireThat,64*1024*1024);
const input=JSON.parse(readFileSync(0,'utf8')),trace=[],gates=new Map(),running=new Map();
const owner=new Original();owner.base=new URL('https://example.test/ivy/');owner.bufferedBodies=0;owner.bodies=new Map();
const budgetOptions=input.budget??{};
const budget=new RequestBudget(budgetOptions.maximum??2048,budgetOptions.maximumBytes??128*1024*1024,budgetOptions.perConsumer??64,budgetOptions.perConsumerBytes??64*1024*1024,'request');
const error=spec=>spec==='internal_error'?IvyError.from(new Error('isolated failure')):new IvyError(spec??'unavailable','Isolated provider failure.');
const wire=e=>IvyError.from(e).toWire();
let store,pushStore,coordinator;
if(input.realStore){
 const {HiveStore}=await import('../../services/hive/src/store.ts');
 const {BrowserPushStore}=await import('../../services/hive/src/browser-push-store.ts');
 store=new HiveStore(':memory:');pushStore=new BrowserPushStore(store,owner.base);store.setMetadata('browser_push_keys',JSON.stringify(input.keys));store.db.exec(input.seedSql);
}
owner.worker={async request(payload){
 trace.push({kind:'request',encoded:JSON.stringify(payload)});
 if(store){
  if(payload.action==='browserPush.request')return pushStore.request(payload.context,payload.request);
  if(payload.action==='browserPush.next')return pushStore.next();
  if(payload.action==='browserPush.keys')return pushStore.keys();
  pushStore.complete(payload.id,payload.status);return null;
 }
 if(input.workerError)throw error(input.workerError);
 return input.reply??{ok:true};
}};
if(store){
 const {BrowserPush}=await import('../../services/hive/src/browser-push.ts');
 coordinator=new BrowserPush(owner.worker,owner.base.href,async(subscription,payload,options)=>{trace.push({kind:'send',subscription,payload,options});return{statusCode:201};});
}
owner.browserPush={wake(){trace.push({kind:'wake'});coordinator?.wake();}};
function stream(spec){
 let index=0;
 return{method:'POST',headers:spec.headers??{},[Symbol.asyncIterator](){return this;},async next(){
  const at=index++,item=(spec.chunks??[])[at];trace.push({kind:'read',id:spec.id,index:at,buffered:owner.bufferedBodies});
  if(!item)return{done:true};
  if(item.gate)await new Promise(resolve=>gates.set(item.gate,resolve));
  if(item.error)throw error(item.error);
  const value=item.base64?Buffer.from(item.base64,'base64'):item.fill!==undefined?Buffer.alloc(item.length,item.fill):Buffer.from(item.text??'');
  return{value,done:false};
 },async return(){trace.push({kind:'cancel',id:spec.id,buffered:owner.bufferedBodies});if(spec.cancelError)throw error(spec.cancelError);return{done:true};}};
}
function response(spec){
 return{writableEnded:spec.ended??false,destroyed:spec.destroyed??false,headersSent:spec.headersSent??false,
  writeHead(status,headers){trace.push({kind:'head',id:spec.id,status,headers});this.headersSent=true;if(spec.throwAt==='head')throw error('internal_error');},
  end(text){trace.push({kind:'end',id:spec.id,text});if(spec.throwAt==='end')throw error('internal_error');this.writableEnded=true;},
  destroy(){trace.push({kind:'destroy',id:spec.id});this.destroyed=true;if(spec.throwAt==='destroy')throw error('internal_error');}
 };
}
async function run(spec){
 const request=stream(spec),lease=budget.acquire('http:peer',spec.reserve??0);owner.bodies.set(request,lease);
 const finished=error=>{
  trace.push(error===undefined?{kind:'result',id:spec.id,value:null}:{kind:'result',id:spec.id,error:wire(error)});
  for(const amount of spec.probes??[]){try{lease.grow(amount);trace.push({kind:'probe',id:spec.id,amount,value:null});}catch(e){trace.push({kind:'probe',id:spec.id,amount,error:wire(e)});}}
  owner.bodies.delete(request);lease.release();
  trace.push({kind:'released',id:spec.id,buffered:owner.bufferedBodies});
 };
 if(spec.mode==='body'){
  try{const bytes=await owner.body(request,spec.limit);trace.push({kind:'body',id:spec.id,length:bytes.length});finished();}
  catch(error){finished(error);}
 }else await owner.route(request,response(spec),spec.context??input.context??{},finished);
}
const flush=()=>new Promise(resolve=>setImmediate(resolve));
try{
 for(const step of input.steps){
  if(step.op==='run')running.set(step.id,run(step));
  else if(step.op==='release'){const gate=gates.get(step.gate);if(!gate)throw Error('Missing fixture gate');gates.delete(step.gate);gate();}
  else if(step.op==='mark')trace.push({kind:'mark',name:step.name,buffered:owner.bufferedBodies});
  else if(step.op==='join')await running.get(step.id);
  else throw Error('Unknown fixture command');
  await flush();
 }
 await Promise.all(running.values());if(coordinator)await coordinator.close();await flush();
 if(store)trace.push({kind:'store',subscriptions:store.all('SELECT endpoint,last_error FROM browser_push ORDER BY endpoint'),outbox:store.all('SELECT id,endpoint,attempts,next_attempt FROM browser_push_outbox ORDER BY id')});
 const final=budget.acquire('http:peer',budgetOptions.perConsumerBytes??64*1024*1024);final.release();
 trace.push({kind:'final-budget',buffered:owner.bufferedBodies});
 process.stdout.write(JSON.stringify({trace}));
}finally{store?.close();}
