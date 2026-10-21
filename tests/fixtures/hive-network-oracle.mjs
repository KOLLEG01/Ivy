// Unchanged routing/diagnostic owners and the original WebSocket send method.
// Socket effects are injected: this is a policy oracle, not a listener/backend.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {readFileSync,existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
registerHooks({resolve(specifier,context,next){
 if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const path=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(path)))return next(path.href,context);}
 return next(specifier,context);
},load(url,context,next){
 if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
 return next(url,context);
}});
const {LiveRouting}=await import('../../services/hive/src/live-routing.ts');
const {ToolBindingDiagnostics}=await import('../../services/hive/src/tool-binding-diagnostics.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const {encodeJson}=await import('../../packages/contracts/src/canonical.ts');
const {managementSocketBufferBytes}=await import('../../packages/contracts/src/limits.ts');
const server=readFileSync(new URL('../../services/hive/src/server.ts',import.meta.url),'utf8');
const kernel=readFileSync(new URL('../../services/hive/src/kernel.ts',import.meta.url),'utf8');
function span(source,start,end){const at=source.indexOf(start),stop=source.indexOf(end,at+start.length);if(at<0||stop<0)throw Error('Original network boundary changed: '+start);return source.slice(at,stop);}
const rpc=new Function('IvyError',stripTypeScriptTypes(span(kernel,'export function rpcFailure(', '\nexport function validRequestId(').replace('export ', '')+span(kernel,'export function validRequestId(', '\n}').concat('\n}').replace('export ',''),{mode:'transform'})+';return {rpcFailure,validRequestId};')(IvyError);
const Sender=new Function('IvyError','WebSocket','encodeJson','rpcFailure','validRequestId','managementSocketBufferBytes',stripTypeScriptTypes('class Sender {\n'+span(server,'  private send(', '  private connected(')+'\n}',{mode:'transform'})+';return Sender;')(IvyError,{OPEN:1},encodeJson,rpc.rpcFailure,rpc.validRequestId,managementSocketBufferBytes);
const error=e=>({error:IvyError.from(e).toWire()});
const ok=()=>({ok:true});
const injected=()=>new IvyError('fixture_callback','Isolated callback failed.','unknown');
function routing(input){
 const events=[],calls=new Map(),owner=new LiveRouting(input.validate,(value)=>{events.push(structuredClone(value));if(input.throwRejected)throw injected();}),steps=[];
 for(const step of input.steps){try{let value;
  if(step.op==='update'){owner.update(step);value=ok();}
  else if(step.op==='prepare'){value={value:owner.prepare(step.context,step.params)};if(step.save)calls.set(step.save,value.value);}
  else if(step.op==='dashboard'){value={value:owner.prepareDashboardImage(step.context,step.node,step.args)};if(step.save)calls.set(step.save,value.value);}
  else if(step.op==='complete')value={value:owner.complete(calls.get(step.call),step.result)};
  else if(step.op==='caller'){owner.caller(step.context);value=ok();}
  else if(step.op==='notification'){owner.notification(step.context,step.params);value=ok();}
  else if(step.op==='disconnect')value={value:owner.disconnect(step.node,step.generation)};
  else if(step.op==='suspend'){owner.suspend(step.node,step.generation);value=ok();}
  else if(step.op==='retire'){owner.retireUnconfiguredPrincipals(step.principals);value=ok();}
  else if(step.op==='clear'){owner.clear();value=ok();}
  else throw Error('Unknown routing fixture step');
  steps.push(structuredClone(value));
 }catch(e){steps.push(error(e));}}
 return {steps,events};
}
function diagnostics(input){
 const events=[],effects=[],steps=[],profile={dates:[],iso:[]};let clock=[],reads=[];
 const parse=Date.parse,iso=Date.prototype.toISOString;
 Date.parse=function(value){effects.push({kind:'parse',value});const result=parse(value);profile.dates.push([value,Number.isNaN(result)?null:result]);return result;};
 Date.prototype.toISOString=function(){const value=reads.at(-1);effects.push({kind:'iso',value});try{const result=iso.call(this);profile.iso.push([value,result]);return result;}catch(e){profile.iso.push([value,null]);throw e;}};
 try{
  const owner=new ToolBindingDiagnostics(value=>{events.push(structuredClone(value));effects.push({kind:'record',value:structuredClone(value)});if(input.throwRecord)throw injected();},()=>{if(!clock.length)throw Error('Missing clock input');const value=clock.shift();reads.push(value);effects.push({kind:'now',value});return value;});
  for(const step of input.steps){clock=step.clock?.slice()??[];reads=[];try{
   if(step.op==='restore')owner.restore(step.values);
   else if(step.op==='rejected')owner.rejected(step.target);
   else if(step.op==='completed')owner.completed(step.target);
   else throw Error('Unknown diagnostic fixture step');
   steps.push({...ok(),reads});
  }catch(e){steps.push({...error(e),reads});}}
 }finally{Date.parse=parse;Date.prototype.toISOString=iso;}
 return {value:{steps,events,effects},profile};
}
function send(input){
 const events=[],socket={readyState:input.closed?3:1,bufferedAmount:input.buffered??0,
  terminate(){events.push({kind:'terminate'});},
  send(text,done){events.push({kind:'send',byteLength:Buffer.byteLength(text),sha256:createHash('sha256').update(text).digest('hex')});if(input.throwSend)throw Error('Isolated send failure');done(input.callbackError?Error('Isolated asynchronous send failure'):null);}};
 const value=structuredClone(input.value);
 if(input.nonfinite)value.result=NaN;
 if(input.fillBytes!==undefined){const empty=Buffer.byteLength(JSON.stringify({...value,result:''}));value.result='x'.repeat(input.fillBytes-empty);}
 let failure;try{new Sender().send({socket},value,input.encoded??undefined);}catch(e){failure=IvyError.from(e).toWire();}
 return {...(failure?{error:failure}:{}),events};
}
const rows=JSON.parse(readFileSync(0,'utf8')).map(input=>input.kind==='routing'?routing(input):input.kind==='diagnostics'?diagnostics(input):send(input));
process.stdout.write(JSON.stringify(rows));
