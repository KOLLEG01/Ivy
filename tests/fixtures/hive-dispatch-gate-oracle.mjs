// The original Promise implementation runs with a deterministic timer queue.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
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
const {DispatchGate}=await import('../../services/hive/src/admission.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const number=value=>value&&typeof value==='object'&&'$number' in value
  ? ({nan:NaN,infinity:Infinity,negativeInfinity:-Infinity,negativeZero:-0})[value.$number] : value;
const nativeSetTimeout=globalThis.setTimeout,nativeClearTimeout=globalThis.clearTimeout;
const flush=async()=>{for(let i=0;i<32;i++)await Promise.resolve();};
const error=value=>new IvyError(value.code??'remote_error',
  value.messageUnits?String.fromCharCode(...value.messageUnits):value.message??'Work failed.',
  value.outcome??'unknown',value.details);
const results=[];
try{
 for(const trace of JSON.parse(readFileSync(0,'utf8'))){
  let now=0,next=0;const timers=new Map(),controls=new Map(),dropped=new Set(),events=[];
  globalThis.setTimeout=(callback,delay)=>{
   // Ask this Node version to normalize the delay; internal/timers.insert
   // truncates the resulting _idleTimeout to integer milliseconds.
   const native=nativeSetTimeout(()=>{},number(delay));
   const duration=Math.trunc(native._idleTimeout);nativeClearTimeout(native);
   if(!Number.isSafeInteger(duration)||duration<1)throw Error('Unknown Node timer representation');
   const id=next++;timers.set(id,{at:now+duration,callback});return id;
  };
  globalThis.clearTimeout=id=>timers.delete(id);
  const gate=new DispatchGate(number(trace.options?.maximum),number(trace.options?.waitMs));
  const rows=[];
  for(const step of trace.steps){
   switch(step.op){
    case 'run':{
     let resolve,reject;const input=new Promise((ok,fail)=>{resolve=ok;reject=fail;});
     // A rejected job may never consume its already-settled input.
     void input.catch(()=>{});controls.set(step.id,{resolve,reject});
     const result=gate.run(()=>{events.push(['start',step.id]);return input;});
     void result.then(value=>{if(!dropped.has(step.id))events.push(['ok',step.id,value]);},
      value=>{if(!dropped.has(step.id))events.push(['error',step.id,IvyError.from(value).toWire()]);});
     if(step.immediate)resolve(step.value??null);
     break;
    }
    case 'complete':{
     const control=controls.get(step.id);if(!control)throw Error('Unknown job');
     if(step.error)control.reject(error(step.error));else control.resolve(step.value??null);
     break;
    }
    case 'drop':dropped.add(step.id);break;
    case 'advance':{
     const until=now+step.ms;
     while(true){
      const due=[...timers].filter(([,entry])=>entry.at<=until)
       .sort((a,b)=>a[1].at-b[1].at||a[0]-b[0])[0];
      if(!due)break;
      const [id,entry]=due;now=entry.at;timers.delete(id);entry.callback();await flush();
     }
     now=until;break;
    }
    default:throw Error('Unknown dispatch fixture operation');
   }
   await flush();rows.push(events.splice(0));
  }
  results.push({name:trace.name,rows});
 }
 process.stdout.write(JSON.stringify(results));
}finally{globalThis.setTimeout=nativeSetTimeout;globalThis.clearTimeout=nativeClearTimeout;}
