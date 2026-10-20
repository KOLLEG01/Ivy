// Execute original server lifecycle methods with controlled asynchronous owners.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const path=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(path)))return next(path.href,context);}return next(specifier,context);},load(url,context,next){if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};return next(url,context);}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {encodeJson}=await import('../../packages/contracts/src/canonical-json.ts');
const {DispatchGate}=await import('../../services/hive/src/admission.ts');
const {LiveRouting}=await import('../../services/hive/src/live-routing.ts');
const {ToolBindingDiagnostics}=await import('../../services/hive/src/tool-binding-diagnostics.ts');
const {managementSocketBufferBytes}=await import('../../packages/contracts/src/limits.ts');
const source=readFileSync(new URL('../../services/hive/src/server.ts',import.meta.url),'utf8');
function span(start,end){const at=source.indexOf(start),stop=source.indexOf(end,at+start.length);if(at<0||stop<0)throw Error('Original lifecycle boundary changed: '+start);return source.slice(at,stop);}
let active;
const methods=span('  async start(', '  private headers(')+span('  private requireFresh(', '  private async authenticate(')+span('  private send(', '  private connected(')+span('  private async forward(', '\n}');
const Owner=new Function('IvyError','requireThat','encodeJson','WebSocket','managementSocketBufferBytes','randomUUID','setImmediate',stripTypeScriptTypes('class Owner {\n'+methods+'\n}',{mode:'transform'})+';return Owner;')(IvyError,requireThat,encodeJson,{OPEN:1},managementSocketBufferBytes,()=>active.id(),callback=>active.immediates.push(callback));
const install=new Function('IvyError',stripTypeScriptTypes(span('    this.worker.onRoutingUpdate = ', '    this.server = createServer('),{mode:'transform'}));
const error=value=>new IvyError(value?.code??'fixture_error',value?.message??'Fixture operation failed.',value?.outcome??'unknown',value?.details);
const hash=text=>'sha256:'+createHash('sha256').update(text).digest('hex');
const native={now:Date.now,setTimeout:globalThis.setTimeout,clearTimeout:globalThis.clearTimeout,setInterval:globalThis.setInterval,clearInterval:globalThis.clearInterval,setImmediate:globalThis.setImmediate};
const flush=async()=>{for(let i=0;i<128;i++)await Promise.resolve();await new Promise(resolve=>native.setImmediate(resolve));};
const results=[];
try{
 for(const trace of JSON.parse(readFileSync(0,'utf8'))){
  const events=[],controls=new Map(),counters=new Map(),timers=new Map(),immediates=[],peers=new Map(),dropped=new Set();let elapsed=0,timerId=0,id=0,listenError;
  const owner=new Owner();active={immediates,id(){const value='fixture-'+(++id);events.push({kind:'id',value});return value;}};
  Date.now=()=>10000;
  const timer=(callback,delay,repeat)=>{const duration=delay>=1&&delay<=2147483647?Math.trunc(delay):1;const id=++timerId;timers.set(id,{callback,at:elapsed+duration,duration,repeat});return id;};
  globalThis.setTimeout=(cb,delay)=>timer(cb,delay,false);globalThis.setInterval=(cb,delay)=>timer(cb,delay,true);globalThis.clearTimeout=globalThis.clearInterval=id=>timers.delete(id);
  const post=(kind,event)=>{const n=(counters.get(kind)??0)+1;counters.set(kind,n);const key=kind+':'+n;events.push(event);let resolve,reject;const value=new Promise((ok,fail)=>{resolve=ok;reject=fail;});controls.set(key,{resolve,reject});return value;};
  const fail=key=>{if(trace.fail?.[key])throw error(trace.fail[key]);};
  const callback=work=>{try{work();}catch(e){events.push({kind:'unhandled',channel:'callback',error:IvyError.from(e).toWire()});}};
  const observe=(name,work)=>{void work.then(value=>{if(!dropped.has(name))events.push({kind:'ok',name,...(value===undefined?{}:{value})});},e=>{if(!dropped.has(name))events.push({kind:'failed',name,error:IvyError.from(e).toWire()});});};
  Object.assign(owner,{options:{credentials:trace.credentials??[],...(trace.options??{})},peers:new Set(),forwarded:new Map(),dispatchGate:new DispatchGate(trace.dispatch?.maximum,trace.dispatch?.waitMs),connectionGeneration:0,eventsAvailableThroughSequence:0,eventsAvailabilityScheduled:false,stopped:false,healthTimer:null,recoveryTimer:null});
  Object.defineProperty(owner,'authenticatedSockets',{set(){events.push({kind:'cache-clear'});}});
  owner.worker={start(){return post('start',{kind:'storage-start'});},close(){return post('storage-close',{kind:'storage-close'});},request(payload,timeout=30000,terminate=true,reserved=4096){return post(payload.action,{kind:'worker',payload:structuredClone(payload),options:{timeoutMs:timeout,terminateOnTimeout:terminate,reservedBytes:reserved,backup:false}});}};
  owner.bindingDiagnostics=new ToolBindingDiagnostics(value=>events.push({kind:'binding-record',value:structuredClone(value)}),()=>10000);
  owner.routing=new LiveRouting(false,target=>owner.bindingDiagnostics.rejected(target));
  owner.oauth={initialize(value){events.push({kind:'oauth-initialize',value:structuredClone(value)});fail('oauth-initialize');},setCredentials(value){events.push({kind:'oauth-set',value:structuredClone(value)});fail('oauth-set');}};
  owner.packages=trace.packages?{initialize(){return post('packages-initialize',{kind:'packages-initialize'});},collectArtifacts(){return post('packages-collect',{kind:'packages-collect'});}}:null;
  owner.browserPush={start(){events.push({kind:'push-start'});fail('push-start');},close(){return post('push-close',{kind:'push-close'});},wake(){events.push({kind:'wake'});}};
  owner.server={once(name,handler){if(name!=='error')throw Error('Unexpected listen event');listenError=handler;events.push({kind:'listen-error-on'});},off(name,handler){if(name!=='error'||listenError!==handler)throw Error('Unexpected listen removal');events.push({kind:'listen-error-off'});},listen(port,host,ready){void post('listen',{kind:'listen',port,host}).then(()=>ready(),reason=>listenError(reason));},address(){events.push({kind:'address'});fail('address');return Object.hasOwn(trace,'address')?trace.address:{port:39082,address:'127.0.0.1'};},closeAllConnections(){events.push({kind:'connections-close'});fail('connections-close');},close(done){void post('http-close',{kind:'http-close'}).then(()=>done(),reason=>done(reason));}};
  owner.ws={close(){events.push({kind:'ws-close'});fail('ws-close');}};
  install.call(owner,IvyError);
  const rows=[];
  for(const step of trace.steps){
   try{switch(step.op){
    case 'start':observe(step.name,owner.start());break;
    case 'close':observe(step.name,owner.close());break;
    case 'credentials':observe(step.name,owner.setCredentials(step.value));break;
    case 'collect':observe(step.name,owner.collectPackageArtifacts());break;
    case 'snapshot':events.push({kind:'credentials',value:structuredClone(owner.options.credentials)});break;
    case 'drop':dropped.add(step.name);break;
    case 'resolve':{const control=controls.get(step.key);if(!control)throw Error('Missing control '+step.key);controls.delete(step.key);control.resolve(step.value);break;}
    case 'reject':{const control=controls.get(step.key);if(!control)throw Error('Missing control '+step.key);controls.delete(step.key);control.reject(error(step.error));break;}
    case 'recover':owner.scheduleRecovery();break;
    case 'failure':callback(()=>owner.worker.onFailure());break;
    case 'routing':callback(()=>owner.worker.onRoutingUpdate(step.value));break;
    case 'events':callback(()=>owner.worker.onEventsAvailable(step.through));break;
    case 'changes':callback(()=>owner.worker.onChanges(step.scopes));break;
    case 'immediates':for(const work of immediates.splice(0))callback(work);break;
    case 'diagnostic-completed':owner.bindingDiagnostics.completed(step.resource);break;
    case 'route':events.push({kind:'prepared',value:owner.routing.prepare(step.context,step.params)});break;
    case 'connect':{const context=structuredClone(step.context),state={open:true};const socket={get readyState(){return state.open?1:3;},get bufferedAmount(){return 0;},send(text,done){events.push({kind:'send',peer:step.name,value:JSON.parse(text),byteLength:Buffer.byteLength(text),sha256:hash(text)});done(null);},terminate(){events.push({kind:'terminate',peer:step.name});fail('terminate');state.open=false;},ping(){events.push({kind:'ping',peer:step.name});fail('ping');}};const peer={socket,context,alive:true,changeScopes:[],notificationFilters:[],chain:new DispatchGate(),active:0,diagnosedAt:0};owner.peers.add(peer);peers.set(step.name,peer);break;}
    case 'pong':peers.get(step.name).alive=true;break;
    case 'forward':observe(step.name,owner.forward(step.context,step.call));break;
    case 'advance':{elapsed+=step.ms;const due=[...timers].filter(([,t])=>t.at<=elapsed).sort((a,b)=>a[1].at-b[1].at||a[0]-b[0]);for(const [id,t] of due){if(!timers.has(id))continue;if(t.repeat)t.at=elapsed+t.duration;else timers.delete(id);callback(t.callback);}break;}
    case 'flush':break;
    default:throw Error('Unknown lifecycle step '+step.op);
   }}catch(e){events.push({kind:'error',error:IvyError.from(e).toWire()});}
   if(!step.noFlush)await flush();rows.push(events.splice(0));
  }
  results.push(rows);
 }
}finally{Date.now=native.now;globalThis.setTimeout=native.setTimeout;globalThis.clearTimeout=native.clearTimeout;globalThis.setInterval=native.setInterval;globalThis.clearInterval=native.clearInterval;}
process.stdout.write(JSON.stringify(results));
