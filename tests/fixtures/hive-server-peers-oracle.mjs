// Original server peer/forwarding methods. Socket, worker and executor effects
// are controlled; this is not a listener or a replacement business backend.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
registerHooks({resolve(specifier,context,next){
 if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const path=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(path)))return next(path.href,context);}
 return next(specifier,context);
},load(url,context,next){if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};return next(url,context);}});
const {IvyError,requireThat,fail}=await import('../../packages/contracts/src/errors.ts');
const {encodeJson}=await import('../../packages/contracts/src/canonical-json.ts');
const {validateRequest,validateResponseFrame,validateInput,validateOutput}=await import('../../packages/contracts/src/core-validation.ts');
const {validateServiceNotificationFrame,validateToolRouting}=await import('../../packages/contracts/src/transport-validation.ts');
const {connectionInFlightRequests,managementSocketBufferBytes}=await import('../../packages/contracts/src/limits.ts');
const {DispatchGate,RequestBudget}=await import('../../services/hive/src/admission.ts');
const {LiveRouting}=await import('../../services/hive/src/live-routing.ts');
const {ToolBindingDiagnostics}=await import('../../services/hive/src/tool-binding-diagnostics.ts');
const {resolveCoreCall}=await import('../../services/hive/src/discovery.ts');
const server=readFileSync(new URL('../../services/hive/src/server.ts',import.meta.url),'utf8'),kernel=readFileSync(new URL('../../services/hive/src/kernel.ts',import.meta.url),'utf8');
function span(source,start,end){const at=source.indexOf(start),stop=source.indexOf(end,at+start.length);if(at<0||stop<0)throw Error('Original peer boundary changed: '+start);return source.slice(at,stop);}
const rpc=new Function('IvyError',stripTypeScriptTypes(span(kernel,'export function rpcFailure(', '\nexport function validRequestId(').replace('export ', '')+span(kernel,'export function validRequestId(', '\n}').concat('\n}').replace('export ',''),{mode:'transform'})+';return {rpcFailure,validRequestId};')(IvyError);
let active;
const body=span(server,'  private requireFresh(', '\n  private ')+span(server,'  private parse(', '  private redirect(')+span(server,'  private checkedDirect(', '  private direct(')+span(server,'  private direct(', '  private subscribed(')+span(server,'  private subscribed(', '  private async upgrade(')+span(server,'  private send(', '\n}')+span(server,'  private eventsAvailable(', '  async close(');
const names=['IvyError','requireThat','fail','WebSocket','encodeJson','rpcFailure','validRequestId','DispatchGate','connectionInFlightRequests','managementSocketBufferBytes','randomUUID','validateRequest','validateResponseFrame','validateInput','validateOutput','validateServiceNotificationFrame','validateToolRouting','resolveCoreCall','setImmediate'];
const Owner=new Function(...names,stripTypeScriptTypes('class Owner {\n'+body+'\n}',{mode:'transform'})+';return Owner;')(IvyError,requireThat,fail,{OPEN:1},encodeJson,rpc.rpcFailure,rpc.validRequestId,DispatchGate,connectionInFlightRequests,managementSocketBufferBytes,()=>active.id(),validateRequest,validateResponseFrame,validateInput,validateOutput,validateServiceNotificationFrame,validateToolRouting,resolveCoreCall,callback=>active.immediates.push(callback));
const providerBody=span(server,'  private async providerResult(', '  private protocolDiagnostic(');
const makeProvider=(constructor,validator)=>new Function('IvyError','validateResponseFrame',stripTypeScriptTypes('class Provider {\n'+providerBody+'\n}',{mode:'transform'})+';return Provider;')(constructor,validator).prototype.providerResult;
const changes=span(server,'    this.worker.onChanges = ', '    this.worker.onFailure = '),failure=span(server,'    this.worker.onFailure = ', '    this.server = createServer(');
const installEffects=new Function('IvyError',stripTypeScriptTypes(changes+failure,{mode:'transform'}));
const installHealth=new Function('setInterval',stripTypeScriptTypes(span(server,'    this.healthTimer = setInterval(', '    const address = this.server.address();'),{mode:'transform'}));
const number=value=>value&&typeof value==='object'&&'$number' in value?({nan:NaN,infinity:Infinity,negativeInfinity:-Infinity,negativeZero:-0})[value.$number]:value;
const error=value=>new IvyError(value.code??'remote_error',value.messageUnits?String.fromCharCode(...value.messageUnits):value.message??'Work failed.',value.outcome??'unknown',value.details);
const hash=text=>'sha256:'+createHash('sha256').update(text).digest('hex');
const decode=value=>value&&typeof value==='object'&&'$repeat' in value?'x'.repeat(value.$repeat):value??null;
const expand=value=>value&&typeof value==='object'?(('$units' in value)?String.fromCharCode(...value.$units):('$repeat' in value)?decode(value):Array.isArray(value)?value.map(expand):Object.fromEntries(Object.entries(value).map(([k,v])=>[k,expand(v)]))):value;
const summary=value=>typeof value==='string'&&value.length>4096?{stringBytes:Buffer.byteLength(value),sha256:hash(value)}:value;
const nativeImmediate=globalThis.setImmediate;
const flush=async()=>{for(let i=0;i<128;i++)await Promise.resolve();await new Promise(resolve=>nativeImmediate(resolve));};
const native={now:Date.now,parse:Date.parse,iso:Date.prototype.toISOString,setTimeout:globalThis.setTimeout,clearTimeout:globalThis.clearTimeout};
const results=[];
try{
 for(const trace of JSON.parse(readFileSync(0,'utf8'))){
  const owner=new Owner(),events=[],controls=new Map(),peers=new Map(),sockets=new Map(),dropped=new Set(),callIds=new Map(),timers=new Map(),immediates=[],profile={dates:[],iso:[]};
  const unhandled=reason=>events.push({kind:'unhandled',channel:'promise',error:IvyError.from(reason).toWire()});process.on('unhandledRejection',unhandled);
  let wall=trace.now??10000,clock=[],elapsed=0,timerId=0,idNumber=0,idInputs=trace.ids?.slice()??[];
  active={immediates,id(){const value=idInputs.length?idInputs.shift():'fixture-'+(++idNumber);events.push({kind:'id',value});return value;}};
  Date.now=()=>{const value=number(clock.length?clock.shift():wall);events.push({kind:'now',value});return value;};
  Date.parse=function(value){events.push({kind:'parse',value});const result=native.parse(value);profile.dates.push([value,Number.isNaN(result)?null:result]);return result;};
  Date.prototype.toISOString=function(){const value=this.getTime();events.push({kind:'iso',value});const result=native.iso.call(this);profile.iso.push([value,result]);return result;};
  globalThis.setTimeout=(callback,delay)=>{const timer=native.setTimeout(()=>{},number(delay)),duration=Math.trunc(timer._idleTimeout);native.clearTimeout(timer);const id=++timerId;timers.set(id,{at:elapsed+duration,callback});return id;};
  globalThis.clearTimeout=id=>timers.delete(id);
  Object.assign(owner,{options:trace.options??{},peers:new Set(),forwarded:new Map(),dispatchGate:new DispatchGate(number(trace.dispatch?.maximum),number(trace.dispatch?.waitMs)),requests:new RequestBudget(number(trace.requests?.maximum),number(trace.requests?.maximumBytes),number(trace.requests?.perConsumer),number(trace.requests?.perConsumerBytes),trace.requests?.name),connectionGeneration:0,eventsAvailableThroughSequence:0,eventsAvailabilityScheduled:false,stopped:false});
  const post=(key,event)=>{events.push(event);if(controls.has(key))throw Error('Reused fixture control '+key);let resolve,reject;const work=new Promise((ok,fail)=>{resolve=ok;reject=fail;});controls.set(key,{resolve,reject});return work;};
  owner.worker={request(payload,timeout=30000,terminate=true,reserved=4096){const event={kind:'worker',payload:structuredClone(payload),options:{timeoutMs:timeout,terminateOnTimeout:terminate,reservedBytes:reserved,backup:false}};
   if(payload.action==='execute'&&typeof payload.request?.params?.label==='string')return post('worker:'+payload.request.params.label,event);events.push(event);return payload.action==='execute'?Promise.reject(new IvyError('invalid_frame','Fixture worker rejected this frame.')):Promise.resolve(null);
  }};
  owner.bindingDiagnostics=new ToolBindingDiagnostics(value=>{events.push({kind:'binding-record',value:structuredClone(value)});if(trace.throwRecord)throw error({code:'fixture_record',message:'Record failed.'});},Date.now);
  owner.routing=new LiveRouting(trace.options?.validateMessages===true,value=>owner.bindingDiagnostics.rejected(value));
  owner.withDispatchGate=work=>owner.dispatchGate.run(work);
  const direct=owner.direct;
  owner.direct=function(context,request,peer){
   if(request?.method==='notifications.subscribe')return direct.call(this,context,request,peer);
   this.requireFresh(context);const p=request?.params;
   if(p?.fixtureDirect==='throw')throw error(p.error??{});
   if(p?.fixtureDirect==='return')return post('direct:'+p.label,{kind:'direct-start',label:p.label});
   return null;
  };
  let providerValid=false;
  function ProviderError(code,message,outcome,details){if(providerValid){events.push({kind:'provider-error-input',code,message,outcome,...(details===undefined?{}:{details})});if(trace.captureErrorFactory)return new IvyError('fixture_factory','Captured canonical error input.','unknown');}return new IvyError(code,message,outcome,details);}
  owner.providerResult=makeProvider(ProviderError,value=>{providerValid=false;validateResponseFrame(value);providerValid=true;});
  owner.browserPush={wake(){events.push({kind:'wake'});}};owner.scheduleRecovery=()=>events.push({kind:'recovery'});
  Object.defineProperty(owner,'authenticatedSockets',{set(){events.push({kind:'cache-clear'});}});
  installEffects.call(owner,IvyError);let health;installHealth.call(owner,callback=>{health=callback;return 1;});
  function socket(name,options={}){
   const state={open:options.open!==false,buffered:options.buffered??0,reads:options.bufferReads?.slice()??[],throwSend:options.throwSend??0,throwTerminate:options.throwTerminate??0,callbackError:options.callbackError??false},handlers=new Map();
   const socket={get readyState(){return state.open?1:3;},get bufferedAmount(){return number(state.reads.length?state.reads.shift():state.buffered);},on(name,callback){handlers.set(name,callback);},
    send(text,callback){events.push({kind:'send',peer:name,byteLength:Buffer.byteLength(text),sha256:hash(text),...(text.length<=4096?{value:JSON.parse(text)}:{})});if(state.throwSend>0){state.throwSend--;throw new Error('Fixture synchronous send failure');}callback(state.callbackError?new Error('Fixture asynchronous send failure'):null);},
    terminate(){events.push({kind:'terminate',peer:name});if(state.throwTerminate>0){state.throwTerminate--;throw Error('Fixture terminate failure');}state.open=false;},ping(){events.push({kind:'ping',peer:name});}};
   sockets.set(name,{socket,state,handlers});return socket;
  }
  const rows=[];
  for(const step of trace.steps){if(step.clock)clock=step.clock.slice();if(step.now!==undefined)wall=step.now;
   const peer=step.peer?peers.get(step.peer):null;
   try{switch(step.op){
    case 'connect':{const s=socket(step.name,step.socket);owner.connected(s,step.context);peers.set(step.name,[...owner.peers].find(p=>p.socket===s));break;}
    case 'update':owner.routing.update({node:step.node,generation:step.generation,catalog:step.catalog});break;
    case 'prepare':events.push({kind:'prepared',value:owner.routing.prepare(step.context,step.params)});break;
    case 'forward':{let before=events.length;const work=owner.forward(step.context,expand(step.call));const generated=events.slice(before).find(e=>e.kind==='id');if(generated)callIds.set(step.name,'hive-call-'+generated.value);void work.then(value=>{if(!dropped.has(step.name))events.push({kind:'forward-ok',name:step.name,value:summary(value)});},e=>{if(!dropped.has(step.name))events.push({kind:'forward-error',name:step.name,error:IvyError.from(e).toWire()});});break;}
    case 'result':{const frame=expand(structuredClone(step.frame));if(step.call)frame.id=callIds.get(step.call);void owner.providerResult(peer,frame);break;}
    case 'message':{let bytes;if(step.bytes)bytes=Buffer.from(step.bytes);else if(step.text!==undefined)bytes=Buffer.from(step.text);else bytes=Buffer.from(JSON.stringify(expand(step.frame)));sockets.get(step.peer).handlers.get('message')(bytes,step.binary??false);break;}
    case 'complete':{const control=controls.get(step.name);if(!control)throw Error('Unknown control '+step.name);if(step.error)control.reject(error(step.error));else control.resolve(step.name.startsWith('worker:')?{kind:step.kind??'result',value:decode(step.value)}:decode(step.value));break;}
    case 'close':{const s=sockets.get(step.peer);s.state.open=false;s.handlers.get('close')();break;}
    case 'pong':sockets.get(step.peer).handlers.get('pong')();break;
    case 'socket':Object.assign(sockets.get(step.peer).state,structuredClone(step.values));break;
    case 'changes':owner.worker.onChanges(step.scopes);break;
    case 'health':health();break;
    case 'events':owner.eventsAvailable(number(step.through));break;
    case 'publish':{for(const callback of immediates.splice(0))callback();break;}
    case 'stop':owner.stopped=true;break;
    case 'shutdown':owner.rejectAll(new IvyError('outcome_unknown','Hive is shutting down; reconcile the original operation.','unknown'));for(const p of owner.peers)p.socket.terminate();break;
    case 'failure':owner.worker.onFailure();break;
    case 'reject':owner.rejectAll(error(step.error??{}));break;
    case 'drop':dropped.add(step.name);break;
    case 'inspect':events.push({kind:'context',peer:step.peer,value:structuredClone(peer.context)});break;
    case 'rejected':owner.bindingDiagnostics.rejected(step.target);break;
    case 'probe':{try{const lease=owner.requests.acquire(step.key,number(step.bytes));lease.release();events.push({kind:'probe',ok:true});}catch(e){events.push({kind:'probe',error:IvyError.from(e).toWire()});}break;}
    case 'advance':{const until=elapsed+step.ms;while(true){const due=[...timers].filter(([,entry])=>entry.at<=until).sort((a,b)=>a[1].at-b[1].at||a[0]-b[0])[0];if(!due)break;const [id,entry]=due;elapsed=entry.at;timers.delete(id);try{entry.callback();}catch(error){events.push({kind:'unhandled',channel:'callback',error:IvyError.from(error).toWire()});}await flush();}elapsed=until;break;}
    default:throw Error('Unknown peer fixture step '+step.op);
   }}catch(e){events.push({kind:'operation-error',error:IvyError.from(e).toWire()});}
   await flush();rows.push(structuredClone(events.splice(0)));
  }
  results.push({value:{name:trace.name,rows},profile});
  process.off('unhandledRejection',unhandled);
 }
 process.stdout.write(JSON.stringify(results));
}finally{Date.now=native.now;Date.parse=native.parse;Date.prototype.toISOString=native.iso;globalThis.setTimeout=native.setTimeout;globalThis.clearTimeout=native.clearTimeout;}
