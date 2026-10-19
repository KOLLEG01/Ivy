// Unchanged original coordinator with explicit worker/sender and timer primitives.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath,pathToFileURL} from 'node:url';
registerHooks({resolve(specifier,context,next){
 if(specifier==='web-push'&&process.env.IVY_HIVE_TEST_WEB_PUSH)return next(pathToFileURL(process.env.IVY_HIVE_TEST_WEB_PUSH).href,context);
 if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
 return next(specifier,context);
},load(url,context,next){
 if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
 return next(url,context);
}});
const {BrowserPush}=await import('../../services/hive/src/browser-push.ts');
const input=JSON.parse(readFileSync(0,'utf8')),trace=[],gates=new Map(),timers=new Map();
const realInterval=globalThis.setInterval,realClear=globalThis.clearInterval,realNow=Date.now;
let now=0,timerId=0,store,pushStore;
globalThis.setInterval=(callback,delay)=>{const timer={id:++timerId,callback,delay,due:now+delay,unref(){this.unreferenced=true;return this;}};timers.set(timer.id,timer);return timer;};
globalThis.clearInterval=timer=>{if(timer)timers.delete(timer.id);};
Date.now=()=>100000;
if(input.realStore){
 const {HiveStore}=await import('../../services/hive/src/store.ts');
 const {BrowserPushStore}=await import('../../services/hive/src/browser-push-store.ts');
 store=new HiveStore(':memory:');pushStore=new BrowserPushStore(store,new URL('https://example.test/ivy/'));
 store.setMetadata('browser_push_keys',JSON.stringify(input.keys));store.db.exec(input.seedSql);
}
const next=[...(input.next??[])],keys=[...(input.keyReplies??[])],sends=[...input.sends];
const reply=async spec=>{
 if(spec.gate)await new Promise(resolve=>gates.set(spec.gate,resolve));
 if(spec.error)throw new Error('isolated worker failure');
 return spec.value??null;
};
const worker={request(payload){
 trace.push({kind:'request',payload});
 if(store){
  try{
   if(payload.action==='browserPush.next')return Promise.resolve(pushStore.next());
   if(payload.action==='browserPush.keys')return Promise.resolve(pushStore.keys());
   pushStore.complete(payload.id,payload.status);return Promise.resolve(null);
  }catch(error){return Promise.reject(error);}
 }
 if(payload.action==='browserPush.next')return reply(next.shift()??{value:{deliveries:[],hasMore:false}});
 if(payload.action==='browserPush.keys')return reply(keys.shift()??{value:input.keys});
 return reply(input.completions?.[payload.id]??{value:null});
}};
const send=(subscription,payload,options)=>{
 trace.push({kind:'send',subscription,payload,options});
 const spec=sends.shift();if(!spec)throw new Error('Unexpected fixture delivery');
 return (async()=>{
  if(spec.gate)await new Promise(resolve=>gates.set(spec.gate,resolve));
  if(spec.kind==='nullish')throw null;
  const value=Object.hasOwn(spec,'status')?{statusCode:spec.status}:{};
  if(spec.kind==='error')throw value;
  return value;
 })();
};
const flush=()=>new Promise(resolve=>setImmediate(resolve));
const coordinator=new BrowserPush(worker,input.subject,send),closing=[];
try{
 for(const step of input.steps){
  switch(step.op){
   case 'wake':for(let i=0;i<(step.count??1);i++)coordinator.wake();break;
   case 'start':coordinator.start();break;
   case 'advance':now+=step.ms;for(const timer of [...timers.values()])if(timer.due<=now){timer.due=now+timer.delay;timer.callback();}break;
   case 'release':{const release=gates.get(step.gate);if(!release)throw new Error('Unknown gate '+step.gate);gates.delete(step.gate);release();break;}
   case 'close':closing.push(coordinator.close().then(()=>trace.push({kind:'closed'})));break;
   case 'abandonClose':void coordinator.close();break;
   case 'mark':trace.push({kind:'mark',name:step.name});break;
   default:throw new Error('Unknown fixture command');
  }
  await flush();
 }
 await Promise.all(closing);await flush();
 if(store)trace.push({kind:'store',subscriptions:store.all('SELECT endpoint,last_error FROM browser_push ORDER BY endpoint'),outbox:store.all('SELECT id,endpoint,attempts,next_attempt FROM browser_push_outbox ORDER BY id')});
 process.stdout.write(JSON.stringify({trace,timerEvidence:[...timers.values()].map(t=>({delay:t.delay,unreferenced:t.unreferenced}))}));
}finally{globalThis.setInterval=realInterval;globalThis.clearInterval=realClear;Date.now=realNow;store?.close();}
