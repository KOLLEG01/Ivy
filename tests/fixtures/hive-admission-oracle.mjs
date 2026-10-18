// Exercise the unchanged admission classes with captured numeric inputs and clocks.
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
const {AuthenticationRate,RequestBudget}=await import('../../services/hive/src/admission.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const number=value=>value&&typeof value==='object'&&'$number' in value
  ? ({nan:NaN,infinity:Infinity,negativeInfinity:-Infinity,negativeZero:-0})[value.$number] : value;
const text=(object,name)=>object[name+'Units']?String.fromCharCode(...object[name+'Units']):object[name];
const rate=options=>new AuthenticationRate(...['windowMs','maximum','perClient','clientMaximum'].map(name=>number(options[name])));
const budget=options=>new RequestBudget(...['maximum','maximumBytes','perConsumer','perConsumerBytes'].map(name=>number(options[name])),text(options,'name'));
let now=1000;
const actualNow=Date.now;Date.now=()=>now;
try{
 const results=[];
 for(const trace of JSON.parse(readFileSync(0,'utf8'))){
  const leases=new Map();let current=trace.kind==='rate'?rate(trace.options??{}):budget(trace.options??{});
  const rows=[];
  for(const step of trace.steps){
   if(['grow','release','drop'].includes(step.op)&&!leases.has(step.id)){
    if(!step.optional)throw Error('Fixture references an unavailable lease: '+step.id);
    rows.push({skipped:true});continue;
   }
   try{
    switch(step.op){
     case 'clock':now=number(step.now);break;
     case 'admit':current.admit(text(step,'key'),number(step.now));break;
     case 'acquire':{
      if(leases.has(step.id))throw Error('Fixture reuses a lease identity');
      leases.set(step.id,current.acquire(text(step,'key'),number(step.bytes)));break;
     }
     case 'grow':leases.get(step.id).grow(number(step.bytes));break;
     case 'release':leases.get(step.id).release();break;
     case 'drop':leases.delete(step.id);break;
     case 'reset':current=trace.kind==='rate'?rate(step.options??{}):budget(step.options??{});break;
     default:throw Error('Unknown trace operation');
    }
    rows.push({ok:true});
   }catch(error){rows.push({error:IvyError.from(error).toWire()});}
  }
  results.push({name:trace.name,rows});
 }
 process.stdout.write(JSON.stringify(results));
}finally{Date.now=actualNow;}
