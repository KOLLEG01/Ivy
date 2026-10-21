// Original metadata/registration spans; outer HTTP admission, streams and socket
// I/O are explicit owners. Unselected paths return a fixture-only match flag.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {EventEmitter} from 'node:events';
import {isIP} from 'node:net';
registerHooks({resolve(specifier,context,next){
 if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
 return next(specifier,context);
},load(url,context,next){
 if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};return next(url,context);
}});
const {IvyError,requireThat,fail}=await import('../../packages/contracts/src/errors.ts');
const {digest}=await import('../../packages/contracts/src/canonical.ts');
const {RequestBudget,AuthenticationRate}=await import('../../services/hive/src/admission.ts');
const {HiveOAuth}=await import('../../services/hive/src/oauth.ts');
const source=readFileSync(new URL('../../services/hive/src/server.ts',import.meta.url),'utf8');
function span(start,end){const at=source.indexOf(start),stop=source.indexOf(end,at+start.length);if(at<0||stop<0)throw Error('Original OAuth HTTP span unavailable');return source.slice(at,stop);}
const methods=span('  private json(', '  private async body(')+span('  private async body(', '  private redirect(')+span('  private sessionToken(', '  private async http(');
const root=span('      if (\n        request.method === "GET" &&\n        url.pathname === "/.well-known/oauth-authorization-server"','      requireThat(\n        url.pathname === this.basePath');
const metadata=span('      if (\n        request.method === "GET" &&\n        (path === "/.well-known/oauth-protected-resource"','      if (request.method === "POST" && path === "/oauth/register")');
const registration=span('      if (request.method === "POST" && path === "/oauth/register")','      if (request.method === "GET" && path === "/oauth/authorize")');
const Factory=new Function('IvyError','requireThat','fail','digest','isIP','MAX_FRAME',stripTypeScriptTypes(`class Original {${methods} async route(request,response,pathname,finished){let failure,matched=true;try{const url={pathname};${root}if(pathname!==this.basePath&&!pathname.startsWith(this.basePath+'/')){matched=false;return;}const path=pathname.slice(this.basePath.length)||'/';${metadata}${registration}matched=false;}catch(error){failure=error;}finally{finished(failure,matched);}}}`,{mode:'transform'})+';return Original;');
const Original=Factory(IvyError,requireThat,fail,digest,isIP,64*1024*1024);
const input=JSON.parse(readFileSync(0,'utf8')),trace=[],gates=new Map(),running=new Map(),owner=new Original();
owner.basePath=input.basePath??'/ivy';owner.base=new URL('https://example.test'+owner.basePath+'/');owner.options={trustedProxyAddresses:input.trusted??[]};
owner.bodies=new Map();owner.bufferedBodies=0;owner.authenticatedSockets=new WeakMap();owner.arrivals=new AuthenticationRate();owner.authentication=new RequestBudget(512,512*1024,64,64*1024,'authentication');
const unused=()=>{throw Error('Unexpected provider');};owner.oauth=new HiveOAuth(owner.base,[],{create:unused,rotate:unused});owner.worker={request:unused};
const socket=new EventEmitter();socket.remoteAddress=input.remoteAddress??'192.0.2.1';
const options=input.budget??{},budget=new RequestBudget(options.maximum??2048,options.maximumBytes??128*1024*1024,options.perConsumer??128,options.perConsumerBytes??64*1024*1024,'request');
const error=code=>code==='internal_error'?IvyError.from(new Error('isolated failure')):new IvyError(code,'Isolated provider failure.');
const wire=e=>IvyError.from(e).toWire();
let now=100000;const originalNow=Date.now;Date.now=()=>now;
function probe(spec,phase){try{const request={socket,headers:spec.headers??{}},key=owner.clientAddress(request);owner.arrivals.admit(key);const lease=owner.authentication.acquire(key);lease.release();trace.push({kind:'auth-probe',id:spec.id,phase,value:null});}catch(e){trace.push({kind:'auth-probe',id:spec.id,phase,error:wire(e)});}}
function stream(spec){let index=0;return{method:'method' in spec?spec.method:'POST',headers:spec.headers??{},socket,[Symbol.asyncIterator](){return this;},async next(){const at=index++,item=(spec.chunks??[])[at];trace.push({kind:'read',id:spec.id,index:at,buffered:owner.bufferedBodies});if(!item)return{done:true};if(item.gate)await new Promise(resolve=>gates.set(item.gate,resolve));if(item.error)throw error(item.error);return{done:false,value:item.base64?Buffer.from(item.base64,'base64'):item.fill!==undefined?Buffer.alloc(item.length,item.fill):Buffer.from(item.text??'')};},async return(){trace.push({kind:'cancel',id:spec.id,buffered:owner.bufferedBodies});if(spec.cancelError)throw error(spec.cancelError);return{done:true};}};}
function response(spec){return{writableEnded:spec.ended??false,destroyed:spec.destroyed??false,headersSent:spec.headersSent??false,
 writeHead(status,headers){trace.push({kind:'head',id:spec.id,status,headers});this.headersSent=true;if(spec.probeAt==='head')probe(spec,'head');if(spec.throwAt==='head')throw error('internal_error');},
 end(text){trace.push({kind:'end',id:spec.id,text});if(spec.probeAt==='end')probe(spec,'end');if(spec.throwAt==='end')throw error('internal_error');this.writableEnded=true;},
 destroy(){trace.push({kind:'destroy',id:spec.id});this.destroyed=true;if(spec.probeAt==='destroy')probe(spec,'destroy');if(spec.throwAt==='destroy')throw error('internal_error');}};}
async function run(spec){const request=stream(spec),lease=budget.acquire('http:peer',spec.reserve??0);owner.bodies.set(request,lease);const finished=(error,matched)=>{trace.push(error===undefined?{kind:'result',id:spec.id,value:matched}:{kind:'result',id:spec.id,error:wire(error)});for(const amount of spec.probes??[]){try{lease.grow(amount);trace.push({kind:'probe',id:spec.id,amount,value:null});}catch(e){trace.push({kind:'probe',id:spec.id,amount,error:wire(e)});}}owner.bodies.delete(request);lease.release();trace.push({kind:'released',id:spec.id,buffered:owner.bufferedBodies});};await owner.route(request,response(spec),spec.pathname,finished);}
const flush=()=>new Promise(resolve=>setImmediate(resolve));
try{for(const step of input.steps){
 if(step.op==='run')running.set(step.id,run(step));
 else if(step.op==='join')await running.get(step.id);
 else if(step.op==='release'){const release=gates.get(step.gate);if(!release)throw Error('Missing fixture gate');gates.delete(step.gate);release();}
 else if(step.op==='probe')probe(step,'explicit');
 else if(step.op==='clock')now=step.now;
 else if(step.op==='mark')trace.push({kind:'mark',name:step.name,buffered:owner.bufferedBodies});
 else if(step.op==='auth'){try{const context=await owner.authenticateBounded({method:'GET',headers:{authorization:'Bearer credential'},socket},'http');trace.push({kind:'auth',id:step.id,encoded:JSON.stringify(context)});}catch(e){trace.push({kind:'auth',id:step.id,error:wire(e)});}}
 else throw Error('Unknown fixture operation');await flush();
}await Promise.all(running.values());await flush();const final=budget.acquire('http:peer',options.perConsumerBytes??64*1024*1024);final.release();trace.push({kind:'final-budget',buffered:owner.bufferedBodies});process.stdout.write(JSON.stringify({trace}));}finally{Date.now=originalNow;}
