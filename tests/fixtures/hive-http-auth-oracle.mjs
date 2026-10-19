// Exact original ingress methods, with explicit sockets/OAuth/worker/clock owners.
// Store integration executes the unchanged original worker handle over real SQLite.
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
const source=readFileSync(new URL('../../services/hive/src/server.ts',import.meta.url),'utf8');
function span(start,end){const at=source.indexOf(start),stop=source.indexOf(end,at+start.length);if(at<0||stop<0)throw Error('Original authentication span unavailable');return source.slice(at,stop);}
const methods=span('  private sameOrigin(', '  private async body(')+span('  private sessionToken(', '  private async http(');
const Factory=new Function('IvyError','requireThat','digest','isIP',stripTypeScriptTypes(`class Original {${methods}}`,{mode:'transform'})+';return Original;');
const Original=Factory(IvyError,requireThat,digest,isIP);
const validationAt=source.indexOf('(options.trustedProxyAddresses ?? []).every(');
if(validationAt<0)throw Error('Original proxy validation unavailable');
const validation=source.slice(source.lastIndexOf('    requireThat(',validationAt),source.indexOf('    requireThat(',validationAt));
const validate=new Function('requireThat','isIP','options',stripTypeScriptTypes(validation,{mode:'transform'}));
const input=JSON.parse(readFileSync(0,'utf8')),events=[],results={},snapshots=[],sockets=new Map(),running=new Map(),gates=new Map();
const text=(value,name)=>value[name+'Units']?String.fromCharCode(...value[name+'Units']):value[name];
const headers=value=>{const out={...value};for(const name of ['authorization','cookie'])if(value?.[name+'Units']){out[name]=text(value,name);delete out[name+'Units'];}return out;};
const error=code=>code==='internal_error'?IvyError.from(new Error('isolated failure')):new IvyError(code,'Isolated provider failure.');
const record=(id,value,failure)=>{results[id]=failure?{error:IvyError.from(failure).toWire()}:{encoded:JSON.stringify(value)};};
let now=100000,store,workerHandle;
const realNow=Date.now;Date.now=()=>now;
const owner=new Original();owner.base=new URL('https://example.test/ivy/');owner.basePath=input.basePath??'/ivy';owner.options={trustedProxyAddresses:input.trusted??[]};
owner.authenticatedSockets=new WeakMap();owner.authentication=new RequestBudget(512,512*1024,64,64*1024,'authentication');owner.arrivals=new AuthenticationRate();
const oauthReplies=[...(input.oauthReplies??[])],workerReplies=[...(input.workerReplies??[])];
owner.oauth={authenticate(token,resource){events.push({kind:'oauth',encoded:JSON.stringify([token,resource??null])});const spec=oauthReplies.shift()??{};if(spec.error)throw error(spec.error);return spec.value??null;}};
try{
 if(input.mode==='ip'){
  const rows=[];for(const candidate of input.candidates){try{validate(requireThat,isIP,{trustedProxyAddresses:[candidate]});rows.push({valid:true});}catch(e){rows.push({error:IvyError.from(e).toWire()});}}
  process.stdout.write(JSON.stringify({rows}));
 }else{
  try{validate(requireThat,isIP,owner.options);}catch(e){process.stdout.write(JSON.stringify({constructionError:IvyError.from(e).toWire()}));process.exit(0);}
  if(input.realStore){
   const {HiveStore}=await import('../../services/hive/src/store.ts');store=new HiveStore(':memory:');store.db.exec(input.seedSql);
   const worker=readFileSync(new URL('../../services/hive/src/worker.ts',import.meta.url),'utf8'),start=worker.indexOf('const oauthPrefix = '),end=worker.indexOf('let chain = Promise.resolve();');
   if(start<0||end<start)throw Error('Original worker identity span unavailable');
   const create=new Function('kernel','canonical','digest','createHmac','randomBytes','IvyError','requireThat','browserPush',stripTypeScriptTypes(worker.slice(start,end),{mode:'strip'})+';return handle;');
   workerHandle=create({store},canonical,digest,createHmac,randomBytes,IvyError,requireThat,undefined);
  }
  owner.worker={async request(payload){
   events.push({kind:'worker',encoded:JSON.stringify(payload)});
   if(workerHandle)return workerHandle(payload);
   const spec=workerReplies.shift()??{value:payload.action==='authenticate'?{principalId:'alpha'}:{credentialDigest:'session-credential',sessionDigest:payload.sessionDigest,principalId:'alpha'}};
   if(spec.gate)await new Promise(resolve=>gates.set(spec.gate,resolve));
   if(spec.error)throw error(spec.error);
   return spec.value??null;
  }};
  const flush=()=>new Promise(resolve=>setImmediate(resolve));
  for(const step of input.steps){
   if(step.op==='connect'){const socket=new EventEmitter();socket.remoteAddress=step.remoteAddress;sockets.set(step.id,socket);}
   else if(step.op==='auth'||step.op==='abandon'){
    const request={method:step.method??'GET',headers:headers(step.headers??{}),socket:sockets.get(step.socket)};
    const work=owner.authenticateBounded(request,step.transport??'http',step.resource);
    if(step.op==='abandon')void work.catch(()=>{});
    else running.set(step.id,work.then(value=>record(step.id,value),e=>record(step.id,undefined,e)));
   }else if(step.op==='join'){for(const id of step.ids)await running.get(id);}
   else if(step.op==='release'){const release=gates.get(step.gate);if(!release)throw Error('Missing fixture gate');gates.delete(step.gate);release();}
   else if(step.op==='clock')now=step.now;
   else if(step.op==='reset')owner.authenticatedSockets=new WeakMap();
   else if(step.op==='close')sockets.get(step.socket).emit('close');
   else if(step.op==='sql')store.db.exec(step.sql);
   else if(step.op==='snapshot'){snapshots.push({name:step.name,events:events.splice(0),results:structuredClone(results)});}
   else {
    try{
     let value;
     if(step.op==='token')value=owner.sessionToken({headers:headers(step.headers??{})});
     else if(step.op==='cookie')value=owner.cookie(text(step,'value'),step.clear??false);
     else if(step.op==='address')value=owner.clientAddress({socket:sockets.get(step.socket),headers:headers(step.headers??{})});
     else if(step.op==='origin'){owner.sameOrigin({headers:step.headers??{}});value=null;}
     else if(step.op==='mcpOrigin'){owner.mcpOrigin({headers:step.headers??{}});value=null;}
     else throw Error('Unknown fixture command');
     record(step.id,value);
    }catch(e){record(step.id,undefined,e);}
   }
   if(!step.noFlush)await flush();
  }
  await Promise.all(running.values());await flush();
  snapshots.push({name:'final',events,results});process.stdout.write(JSON.stringify({snapshots}));
 }
}finally{Date.now=realNow;store?.close();}
