// Unchanged server invoke/batch methods, with controlled direct and worker owners.
// This qualifies admission and reply composition, not HTTP or provider execution.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
registerHooks({resolve(specifier,context,next){
 if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){
  const path=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
  if(existsSync(fileURLToPath(path)))return next(path.href,context);
 }
 return next(specifier,context);
},load(url,context,next){
 if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
 return next(url,context);
}});
const {RequestBudget,DispatchGate}=await import('../../services/hive/src/admission.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {encodeJson}=await import('../../packages/contracts/src/canonical-json.ts');
const {validateRequest}=await import('../../packages/contracts/src/core-validation.ts');
const {managementFrameBytes:MAX_FRAME,rpcBatchRequests,rpcBatchRequestBytes}=await import('../../packages/contracts/src/limits.ts');
const server=readFileSync(new URL('../../services/hive/src/server.ts',import.meta.url),'utf8');
const kernel=readFileSync(new URL('../../services/hive/src/kernel.ts',import.meta.url),'utf8');
function span(source,start,end){const at=source.indexOf(start),stop=source.indexOf(end,at+start.length);if(at<0||stop<0)throw Error('Original server boundary changed: '+start);return source.slice(at,stop);}
const rpc=new Function('IvyError',stripTypeScriptTypes(span(kernel,'export function rpcFailure(', '\nexport function validRequestId(').replace('export ', '')+span(kernel,'export function validRequestId(', '\n}').concat('\n}').replace('export ',''),{mode:'transform'})+';return {rpcFailure,validRequestId};')(IvyError);
const methods=span(server,'  private requireFresh(', '\n  private ')+span(server,'  async invoke(', '  private checkedDirect(');
const Owner=new Function('IvyError','requireThat','validateRequest','encodeJson','rpcFailure','validRequestId','MAX_FRAME','rpcBatchRequests','rpcBatchRequestBytes',stripTypeScriptTypes('class Owner {\n'+methods+'\n}',{mode:'transform'})+';return Owner;')(IvyError,requireThat,validateRequest,encodeJson,rpc.rpcFailure,rpc.validRequestId,MAX_FRAME,rpcBatchRequests,rpcBatchRequestBytes);
const number=value=>value&&typeof value==='object'&&'$number' in value?({nan:NaN,infinity:Infinity,negativeInfinity:-Infinity,negativeZero:-0})[value.$number]:value;
const failure=value=>new IvyError(value.code??'remote_error',value.messageUnits?String.fromCharCode(...value.messageUnits):value.message??'Work failed.',value.outcome??'unknown',value.details);
const value=input=>input&&typeof input==='object'&&'$repeat' in input?'x'.repeat(input.$repeat):input??null;
const hash=text=>'sha256:'+createHash('sha256').update(text).digest('hex');
const summarize=result=>typeof result==='string'&&result.length>4096?{stringBytes:Buffer.byteLength(result),sha256:hash(result)}:result;
const batchSummary=body=>({byteLength:Buffer.byteLength(body),sha256:hash(body),items:JSON.parse(body).map(item=>'result' in item?{...item,result:summarize(item.result)}:item)});
const flush=async()=>{for(let i=0;i<64;i++)await Promise.resolve();};
const nativeNow=Date.now,nativeSetTimeout=globalThis.setTimeout,nativeClearTimeout=globalThis.clearTimeout;
const results=[];
try{
 for(const trace of JSON.parse(readFileSync(0,'utf8'))){
  const events=[],controls=new Map(),dropped=new Set(),timers=new Map(),rows=[];
  let monotonic=0,timerId=0,clock=[],wall=trace.now??0;
  Date.now=()=>{const now=number(clock.length?clock.shift():wall);events.push({kind:'now',value:now});return now;};
  globalThis.setTimeout=(callback,delay)=>{
   const native=nativeSetTimeout(()=>{},number(delay)),duration=Math.trunc(native._idleTimeout);nativeClearTimeout(native);
   if(!Number.isSafeInteger(duration)||duration<1)throw Error('Unknown Node timer representation');
   const id=++timerId;timers.set(id,{at:monotonic+duration,callback});return id;
  };
  globalThis.clearTimeout=id=>timers.delete(id);
  const options=trace.requests??{},gate=trace.dispatch??{},owner=new Owner();
  owner.options=trace.options??{};
  owner.requests=new RequestBudget(number(options.maximum),number(options.maximumBytes),number(options.perConsumer),number(options.perConsumerBytes),options.name);
  owner.dispatchGate=new DispatchGate(number(gate.maximum),number(gate.waitMs));
  owner.withDispatchGate=work=>owner.dispatchGate.run(work);
  const post=(name,event)=>{
   if(controls.has(name))throw Error('Reused fixture control: '+name);
   events.push(event);let resolve,reject;const work=new Promise((ok,fail)=>{resolve=ok;reject=fail;});
   controls.set(name,{resolve,reject});return work;
  };
  owner.direct=(context,request)=>{
   owner.requireFresh(context);events.push({kind:'direct',context,request});
   const params=request?.params,mode=params?.fixtureDirect;
   if(mode==='throw')throw failure(params.error??{});
   if(mode==='return')return post('direct:'+params.label,{kind:'direct-start',label:params.label});
   return null;
  };
  owner.worker={request(payload,timeout=30000,terminate=true,reserved=4096){
   return post('storage:'+payload.request.params.label,{kind:'storage',payload,options:{timeoutMs:timeout,terminateOnTimeout:terminate,reservedBytes:reserved,backup:false}});
  }};
  for(const step of trace.steps){
   if(step.clock)clock=step.clock.slice();
   if(step.now!==undefined)wall=step.now;
   switch(step.op){
    case 'invoke':case 'batch':{
     const batch=step.op==='batch',work=batch?owner.invokeBatch(step.context,step.frames,number(step.bytes??0)):owner.invoke(step.context,step.request,number(step.bytes??0));
     void work.then(result=>{if(!dropped.has(step.name))events.push({kind:batch?'batch':'ok',name:step.name,value:batch?batchSummary(result):summarize(result)});},error=>{if(!dropped.has(step.name))events.push({kind:'error',name:step.name,error:IvyError.from(error).toWire()});});
     break;
    }
    case 'complete':{
     const control=controls.get(step.name);if(!control)throw Error('Unknown fixture control: '+step.name);
     if(step.error)control.reject(failure(step.error));else control.resolve(step.name.startsWith('storage:')?{kind:'result',value:value(step.value)}:value(step.value));
     break;
    }
    case 'drop':dropped.add(step.name);break;
    case 'probe':{
     try{const lease=owner.requests.acquire(step.key,number(step.bytes));lease.release();events.push({kind:'probe',ok:true});}
     catch(error){events.push({kind:'probe',error:IvyError.from(error).toWire()});}
     break;
    }
    case 'advance':{
     const until=monotonic+step.ms;
     while(true){const due=[...timers].filter(([,entry])=>entry.at<=until).sort((a,b)=>a[1].at-b[1].at||a[0]-b[0])[0];if(!due)break;
      const [id,entry]=due;monotonic=entry.at;timers.delete(id);entry.callback();await flush();}
     monotonic=until;break;
    }
    default:throw Error('Unknown server RPC fixture step: '+step.op);
   }
   await flush();rows.push(structuredClone(events.splice(0)));
  }
  results.push({name:trace.name,rows});
 }
 process.stdout.write(JSON.stringify(results));
}finally{Date.now=nativeNow;globalThis.setTimeout=nativeSetTimeout;globalThis.clearTimeout=nativeClearTimeout;}
