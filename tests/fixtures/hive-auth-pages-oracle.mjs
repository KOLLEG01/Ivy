// Original renderers, security headers and selected authentication-page policy.
// Canonical wire forms and final redirects use the real URL and OAuth owners.
import {registerHooks,stripTypeScriptTypes,syncBuiltinESMExports} from 'node:module';
import {readFileSync,existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {EventEmitter} from 'node:events';
import {isIP} from 'node:net';
import crypto from 'node:crypto';
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}return next(specifier,context);},load(url,context,next){if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};return next(url,context);}});
const decode=value=>value&&typeof value==='object'&&value.$units?String.fromCharCode(...value.$units):Array.isArray(value)?value.map(decode):value&&typeof value==='object'?Object.fromEntries(Object.entries(value).map(([k,v])=>[k,decode(v)])):value;
const input=decode(JSON.parse(readFileSync(0,'utf8'))),events=[],values={},results={};
let counter=0,now=100000;
const originalRandom=crypto.randomBytes,originalNow=Date.now;
crypto.randomBytes=length=>{const number=counter++,bytes=Buffer.from(Array.from({length},(_,i)=>(number*17+i)%256));bytes.writeBigUInt64LE(BigInt(number));events.push({kind:'random',length,hex:bytes.toString('hex')});return bytes;};syncBuiltinESMExports();Date.now=()=>now;
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {digest}=await import('../../packages/contracts/src/canonical.ts');
const {RequestBudget,AuthenticationRate}=await import('../../services/hive/src/admission.ts');
const {uiContentPolicy}=await import('../../services/hive/src/ui-security.ts');
const {loginPage,loginThemeScript}=await import('../../services/hive/src/login-page.ts');
const {HiveOAuth,oauthPage}=await import('../../services/hive/src/oauth.ts');
const source=readFileSync(new URL('../../services/hive/src/server.ts',import.meta.url),'utf8');
function span(start,end,at=0){const from=source.indexOf(start,at),to=source.indexOf(end,from+start.length);if(from<0||to<0)throw Error('Page source span missing');return source.slice(from,to);}
const methods=span('  private headers(', '  private async body(')+span('  private async body(', '  private redirect(')+span('  private redirect(', '  private async http(');
const loginAt=source.indexOf('      if (path === "/login" && request.method === "POST")');
let post=span('        const token = form.get("token");','      let context: ConnectionContext;',loginAt);post=post.slice(0,post.lastIndexOf('      }'));
const get=span('      if (path === "/login" && request.method === "GET")','      if (path === "/login" && request.method === "POST")');
const theme=span('      if (path === "/login/theme.js" && request.method === "GET")','      if (path === "/login" && request.method === "GET")');
const authorize=span('      if (request.method === "GET" && path === "/oauth/authorize")','      if (request.method === "POST" && path === "/oauth/authorize")');
const loginWire=span('      if (path === "/login" && request.method === "POST")','      let context: ConnectionContext;');
const authorizeWire=span('      if (request.method === "POST" && path === "/oauth/authorize")','      if (request.method === "POST" && path === "/oauth/token")');
const tokenWire=span('      if (request.method === "POST" && path === "/oauth/token")','      if (request.method === "GET" && path === "/health/live")');
const Factory=new Function('IvyError','requireThat','digest','isIP','uiContentPolicy','loginPage','loginThemeScript','oauthPage','MAX_FRAME',stripTypeScriptTypes(`class Original {${methods} async login(request,response,form){${post}} getLogin(response,form){const request={method:'GET'},path='/login',url={searchParams:form};${get}} theme(response){const request={method:'GET'},path='/login/theme.js';${theme}} authorize(request,response,form){const path='/oauth/authorize',url=new URL('https://example.test/oauth/authorize');url.search=new URLSearchParams(form).toString();${authorize}} async loginWire(request,response){const path='/login';${loginWire}} async authorizeWire(request,response){const path='/oauth/authorize';${authorizeWire}} async tokenWire(request,response){const path='/oauth/token';${tokenWire}}}`,{mode:'transform'})+';return Original;');
const Original=Factory(IvyError,requireThat,digest,isIP,uiContentPolicy,loginPage,loginThemeScript,oauthPage,64*1024*1024),owner=new Original();
owner.basePath=input.basePath??'/ivy';owner.base=new URL('https://example.test'+owner.basePath+'/');owner.options={trustedProxyAddresses:input.trusted??[]};owner.arrivals=new AuthenticationRate();owner.authentication=new RequestBudget(512,512*1024,64,64*1024,'authentication');
owner.bodies=new Map();owner.bufferedBodies=0;
const familyReplies=[...(input.familyReplies??[])];
async function family(action,payload,active){events.push({kind:'family',action,encoded:JSON.stringify(payload),...(active?{activeEncoded:JSON.stringify(active)}:{})});if(!familyReplies.length)throw Error('Unexpected family provider');const spec=familyReplies.shift();if(spec.error)throw error(spec.error);return null;}
owner.oauth=new HiveOAuth(owner.base,input.authorized?[{principalId:'alpha',digest:digest('credential')}]:[],{create:(state,active)=>family('create',state,active),rotate:rotation=>family('rotate',rotation)});
if(input.authorized)owner.oauth.initialize({signingKey:'11'.repeat(32),referenceKey:'22'.repeat(32)});
const replies=[...(input.workerReplies??[])];const error=code=>code==='internal_error'?IvyError.from(new Error('isolated failure')):new IvyError(code,'Isolated provider failure.');
owner.worker={async request(payload){events.push({kind:'worker',encoded:JSON.stringify(payload)});const spec=replies.shift()??{};if(spec.error)throw error(spec.error);return 'value' in spec?spec.value:{sessionToken:'issued-token'};}};
const socket=new EventEmitter();socket.remoteAddress=input.remoteAddress??'192.0.2.1';
const request=spec=>({method:'GET',headers:spec.headers??{},socket});
function probe(spec,phase){try{const key=owner.clientAddress(request(spec));owner.arrivals.admit(key);const lease=owner.authentication.acquire(key);lease.release();events.push({kind:'probe',id:spec.id,phase,value:null});}catch(e){events.push({kind:'probe',id:spec.id,phase,error:IvyError.from(e).toWire()});}}
function response(spec){return{writableEnded:spec.ended??false,destroyed:spec.destroyed??false,headersSent:spec.headersSent??false,
 setHeader(name,value){events.push({kind:'header',id:spec.id,name,encoded:JSON.stringify(value)});if(spec.throwAt===name)throw error('internal_error');},
 writeHead(status,headers){events.push({kind:'head',id:spec.id,status,encoded:JSON.stringify(headers)});this.headersSent=true;if(spec.probeAt==='head')probe(spec,'head');if(spec.throwAt==='head')throw error('internal_error');},
 end(text){events.push({kind:'end',id:spec.id,body:text===undefined?null:Buffer.from(text,'utf8').toString('utf8')});if(spec.probeAt==='end')probe(spec,'end');if(spec.throwAt==='end')throw error('internal_error');this.writableEnded=true;},destroy(){events.push({kind:'destroy',id:spec.id});if(spec.throwAt==='destroy')throw error('internal_error');this.destroyed=true;}};}
function stream(spec){let index=0;const chunks=spec.chunks??[{text:spec.formWire?new URLSearchParams(spec.formWire).toString():spec.body??''}];return{method:'POST',headers:spec.headers??{},socket,[Symbol.asyncIterator](){return this;},async next(){const at=index++,item=chunks[at];events.push({kind:'read',id:spec.id,index:at,buffered:owner.bufferedBodies});if(!item)return{done:true};if(item.error)throw error(item.error);return{done:false,value:item.base64?Buffer.from(item.base64,'base64'):item.fill!==undefined?Buffer.alloc(item.length,item.fill):Buffer.from(item.text??'')};},async return(){events.push({kind:'cancel',id:spec.id,buffered:owner.bufferedBodies});if(spec.cancelError)throw error(spec.cancelError);return{done:true};}};}
const bodies=new RequestBudget(2048,128*1024*1024,128,64*1024*1024,'request');
function resolve(value){if(value&&typeof value==='object'&&value.$ref){const [id,...path]=value.$ref.split('.');return path.reduce((v,k)=>v[k],values[id]);}if(Array.isArray(value))return value.map(resolve);if(value&&typeof value==='object')return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,resolve(v)]));return value;}
const held=[];
try{
 if(input.mode==='render'){const rows=input.cases.map(spec=>({encoded:JSON.stringify(spec.kind==='login'?loginPage(spec.base,spec.target,spec.rejected):oauthPage(spec.base,spec.transaction,spec.client,spec.scope,spec.rejected))}));process.stdout.write(JSON.stringify({rows,script:loginThemeScript}));}
 else{for(const raw of input.steps){const step=resolve(raw);try{let value=null;
  if(step.op==='reserve'){for(let i=0;i<step.count;i++){const key=owner.clientAddress(request(step));owner.arrivals.admit(key);held.push(owner.authentication.acquire(key));}events.push({kind:'reserve',count:step.count});}
  else if(step.op==='release'){for(const lease of held.splice(0))lease.release();events.push({kind:'release'});}
  else if(step.op==='probe')probe(step,'explicit');
  else if(step.op==='register')value=owner.oauth.register(step.body);
  else if(step.op==='begin'){const url=new URL('https://example.test/oauth/authorize');url.search=new URLSearchParams(step.form).toString();value=owner.oauth.begin(url);}
  else if(step.op==='transaction')value=owner.oauth.transaction(step.transaction);
  else{const reply=response(step);owner.headers(reply);if(step.op.endsWith('Wire')){const req=stream(step),lease=bodies.acquire('http:peer',0);owner.bodies.set(req,lease);try{await owner[step.op](req,reply);}finally{owner.bodies.delete(req);lease.release();events.push({kind:'wire-state',id:step.id,buffered:owner.bufferedBodies});}}else if(step.op==='getLoginQuery')owner.getLogin(reply,new URLSearchParams(step.query));else if(step.op==='authorizeQuery')owner.authorize(request(step),reply,new URLSearchParams(step.query));else if(step.op==='getLogin')owner.getLogin(reply,new URLSearchParams(step.form??[]));else if(step.op==='theme')owner.theme(reply);else if(step.op==='login')await owner.login(request(step),reply,new URLSearchParams(step.form??[]));else if(step.op==='authorize')owner.authorize(request(step),reply,step.form??[]);else if(step.op!=='headers')throw Error('Unknown page operation');}
  values[step.id]=value;results[step.id]={encoded:JSON.stringify(value)};
 }catch(e){results[step.id]={error:IvyError.from(e).toWire()};}}process.stdout.write(JSON.stringify({events,results}));}
}finally{Date.now=originalNow;crypto.randomBytes=originalRandom;syncBuiltinESMExports();}
