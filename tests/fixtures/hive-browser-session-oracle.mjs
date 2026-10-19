// Exact original post-parse login/logout policy and authentication helpers.
// Page rendering, response I/O and the live peer set are explicit test owners.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {EventEmitter} from 'node:events';
import {isIP} from 'node:net';
import {createHmac,randomBytes} from 'node:crypto';
registerHooks({resolve(specifier,context,next){
 if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
 return next(specifier,context);
},load(url,context,next){
 if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
 return next(url,context);
}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {digest,canonical}=await import('../../packages/contracts/src/canonical.ts');
const {RequestBudget,AuthenticationRate}=await import('../../services/hive/src/admission.ts');
const decode=value=>{
 if(value&&typeof value==='object'&&value.$units)return String.fromCharCode(...value.$units);
 if(Array.isArray(value))return value.map(decode);
 if(value&&typeof value==='object')return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,decode(v)]));
 return value;
};
const input=decode(JSON.parse(readFileSync(0,'utf8'))),events=[],results={},snapshots=[],sockets=new Map(),running=new Map(),gates=new Map();
const source=readFileSync(new URL('../../services/hive/src/server.ts',import.meta.url),'utf8');
function span(start,end,at=0){const from=source.indexOf(start,at),to=source.indexOf(end,from+start.length);if(from<0||to<0)throw Error('Original session span unavailable');return source.slice(from,to);}
const methods=span('  private sameOrigin(', '  private async body(')+span('  private redirect(', '  private async http(');
const loginAt=source.indexOf('      if (path === "/login" && request.method === "POST")');
let login=span('        const token = form.get("token");','      let context: ConnectionContext;',loginAt);
login=login.slice(0,login.lastIndexOf('      }'));
const logout=span('      if (context.sessionDigest) {','      if (\n        (path === "/api/v1/rpc"');
let pageResponse;
const page=(base,target,rejected)=>{events.push({kind:'page',id:pageResponse.id,base,target,rejected});if(pageResponse.throwAt==='page')throw new Error('isolated callback failure');return 'rejected-page';};
const Factory=new Function('IvyError','requireThat','digest','isIP','loginPage',stripTypeScriptTypes(`class Original {${methods} async login(request,response,form){${login}} async logout(request,response,context){const path='/logout';${logout}}}`,{mode:'transform'})+';return Original;');
const Original=Factory(IvyError,requireThat,digest,isIP,page),owner=new Original();
owner.base=new URL('https://example.test'+(input.basePath??'/ivy'));owner.basePath=input.basePath??'/ivy';owner.options={trustedProxyAddresses:input.trusted??[]};
owner.authenticatedSockets=new WeakMap();owner.authentication=new RequestBudget(512,512*1024,64,64*1024,'authentication');owner.arrivals=new AuthenticationRate();owner.peers=new Set();
owner.oauth={authenticate(token,resource){events.push({kind:'oauth',encoded:JSON.stringify([token,resource??null])});return null;}};
let now=100000,store,handle;
const originalNow=Date.now;Date.now=()=>now;
const error=code=>code==='internal_error'?IvyError.from(new Error('isolated failure')):new IvyError(code,'Isolated provider failure.');
const record=(id,value,failure)=>{results[id]=failure?{error:IvyError.from(failure).toWire()}:{encoded:JSON.stringify(value)};};
const request=spec=>({method:spec.method??'POST',headers:spec.headers??{},socket:sockets.get(spec.socket)});
function probe(spec,phase){
 try{const key=owner.clientAddress(request(spec));owner.arrivals.admit(key);const lease=owner.authentication.acquire(key);lease.release();events.push({kind:'probe',id:spec.id,phase,value:null});}
 catch(e){events.push({kind:'probe',id:spec.id,phase,error:IvyError.from(e).toWire()});}
}
function response(spec){return{
 setHeader(name,value){events.push({kind:'header',id:spec.id,name,valueEncoded:JSON.stringify(value)});if(spec.probeAt==='cookie')probe(spec,'cookie');if(spec.throwAt==='cookie')throw error('internal_error');},
 writeHead(status,headers){events.push({kind:'head',id:spec.id,status,headers});pageResponse=spec;const phase=status===401?'rejected.head':'redirect.head';if(spec.probeAt===phase)probe(spec,phase);if(spec.throwAt===phase)throw error('internal_error');},
 end(text){events.push({kind:'end',id:spec.id,...(text===undefined?{}:{text})});if(spec.probeAt==='end')probe(spec,'end');if(spec.throwAt==='end')throw error('internal_error');},
};}
const allPeers=new Map();
function addPeer(spec){
 const existing=allPeers.get(spec.id);if(existing&&owner.peers.has(existing))return;
 const peer={context:'context' in spec?spec.context:{},socket:{terminate(){events.push({kind:'terminate',id:spec.id});if(spec.error)throw error(spec.error);for(const id of spec.remove??[]){const p=allPeers.get(id);if(p)owner.peers.delete(p);}for(const item of spec.append??[])addPeer(item);}}};
 allPeers.set(spec.id,peer);owner.peers.add(peer);
}
const specs=[...(input.workerReplies??[])];
const flush=()=>new Promise(resolve=>setImmediate(resolve));
try{
 if(input.realStore){
  const {HiveStore}=await import('../../services/hive/src/store.ts');store=new HiveStore(':memory:');store.db.exec(input.seedSql);
  const worker=readFileSync(new URL('../../services/hive/src/worker.ts',import.meta.url),'utf8'),from=worker.indexOf('const oauthPrefix = '),to=worker.indexOf('let chain = Promise.resolve();');
  if(from<0||to<from)throw Error('Original worker session span unavailable');
  handle=new Function('kernel','canonical','digest','createHmac','randomBytes','IvyError','requireThat','browserPush',stripTypeScriptTypes(worker.slice(from,to),{mode:'strip'})+';return handle;')({store},canonical,digest,createHmac,randomBytes,IvyError,requireThat,undefined);
 }
 owner.worker={async request(payload){events.push({kind:'worker',encoded:JSON.stringify(payload)});const spec=specs.shift()??{};
  if(spec.gate)await new Promise(resolve=>gates.set(spec.gate,resolve));if(spec.error)throw error(spec.error);
  if(handle)return handle(payload);
  return 'value' in spec?spec.value:payload.action==='session.create'?{sessionToken:'issued-token'}:payload.action==='authenticate'?{principalId:'alpha'}:{credentialDigest:'credential',sessionDigest:payload.sessionDigest,principalId:'alpha'};
 }};
 for(const peer of input.peers??[])addPeer(peer);
 for(const step of input.steps){
  if(step.op==='connect'){const socket=new EventEmitter();socket.remoteAddress=step.remoteAddress;sockets.set(step.id,socket);}
  else if(step.op==='login'||step.op==='logout'||step.op==='auth'){
   const req=request(step);const work=step.op==='login'?owner.login(req,response(step),new URLSearchParams(step.form)):step.op==='logout'?owner.logout(req,response(step),'context' in step?step.context:{}):owner.authenticateBounded(req,step.transport??'http',step.resource);
   running.set(step.id,work.then(value=>record(step.id,step.op==='auth'?value:null),failure=>record(step.id,undefined,failure)));
  }else if(step.op==='join')for(const id of step.ids)await running.get(id);
  else if(step.op==='release'){const release=gates.get(step.gate);if(!release)throw Error('Missing fixture gate');gates.delete(step.gate);release();}
  else if(step.op==='clock')now=step.now;
  else if(step.op==='probe')probe(step,'explicit');
  else if(step.op==='snapshot')snapshots.push({name:step.name,events:events.splice(0),results:structuredClone(results)});
  else if(step.op==='sql')store.db.exec(step.sql);
  else if(step.op==='returnPath'){try{record(step.id,owner.returnPath(step.value??null));}catch(e){record(step.id,undefined,e);}}
  else throw Error('Unknown fixture operation');
  await flush();
 }
 await Promise.all(running.values());await flush();
 snapshots.push({name:'final',events,results});
 const result={snapshots};if(store)result.store={kind:'store',sessions:store.all('SELECT digest,credential_digest FROM browser_sessions ORDER BY digest')};
 process.stdout.write(JSON.stringify(result));
}finally{Date.now=originalNow;store?.close();}
