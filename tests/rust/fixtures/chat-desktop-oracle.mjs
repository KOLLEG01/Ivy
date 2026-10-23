// Unchanged desktop.ts, actual original journal and explicit process/timer probes.
import {readFileSync,writeFileSync,existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {EventEmitter} from 'node:events';
const root=new URL('../../../',import.meta.url),url=p=>new URL(p,root).href;
const data=s=>'data:text/javascript;base64,'+Buffer.from(stripTypeScriptTypes(s,{mode:'transform'})).toString('base64');
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL?.startsWith('file:')){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return{url:u.href,shortCircuit:true};}return n(s,c);},load(u,c,n){if(u.startsWith('file:')&&u.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(fileURLToPath(u),'utf8'),{mode:'transform'}),shortCircuit:true};return n(u,c);}});
const errors=url('packages/contracts/src/errors.ts'),sdk=data(`export {IvyError,requireThat} from '${errors}';`);
const journal=data(readFileSync(new URL('services/chat-bridge/src/whatsapp/journal.ts',root),'utf8').replaceAll("'../../../../packages/sdk/src/node.js'",JSON.stringify(sdk)));
const {WhatsAppJournal}=await import(journal),{IvyError}=await import(errors),{translator}=await import(url('services/chat-bridge/src/whatsapp/messages.ts'));
const child=data(`export const spawn=(...args)=>globalThis.desktopProbe.spawn(...args);`);
let source=readFileSync(new URL('services/chat-bridge/src/whatsapp/desktop.ts',root),'utf8');
for(const [s,r] of Object.entries({'node:child_process':child,'./journal.js':journal,'../../../../packages/sdk/src/node.js':sdk,'./messages.js':url('services/chat-bridge/src/whatsapp/messages.ts')}))source=source.replaceAll(JSON.stringify(s),JSON.stringify(r));
const {desktopCommand}=await import(data(source));
const snapshot=j=>Object.fromEntries(['kv','inbox','outgoing','retired_inbox','retired_outgoing'].map(t=>[t,j.db.prepare('SELECT * FROM '+t+' ORDER BY '+(t==='kv'?'key':'id')).all().map(r=>Object.values(r))]));
const specs=[],add=(label,patch={})=>specs.push({label,platform:'win32',language:'en',actions:['ensure','ensure'],...patch});
for(const language of ['en','de','fr'])for(const action of ['ensure','restart'])add(language+'-'+action,{language,actions:[action,action]});
for(const platform of ['linux','darwin'])for(const action of ['ensure','restart','invalid'])add(platform+'-'+action,{platform,actions:[action],stored:{attempted:true,result:'cached'}});
for(const action of ['', 'ENSURE','restart; Write-Output injected'])add('invalid-'+action,{actions:[action],stored:{result:'cached'}});
for(const stored of [{attempted:true},{attempted:'yes'},{attempted:1},{attempted:false},{attempted:0},{attempted:null},{result:''},{result:0},{result:false},{result:'cached'},{result:17},{result:{}},{result:[]},true,'stored'])add('stored-'+JSON.stringify(stored),{stored});
for(const event of ['nonzero','signal','spawn-error','timeout','exit-then-error','error-then-exit'])add('event-'+event,{event});
add('cross-action-cache',{actions:['ensure','restart','invalid']});
add('key-is-only-storage',{key:'quoted; $() ` key ß 😀',actions:['restart','ensure']});
const nativePlatform=Object.getOwnPropertyDescriptor(process,'platform'),nativeSet=globalThis.setTimeout,nativeClear=globalThis.clearTimeout;
const rows=[];
try{for(const spec of specs){
 Object.defineProperty(process,'platform',{...nativePlatform,value:spec.platform});
 const j=new WhatsAppJournal(':memory:'),key=spec.key??'original-key',calls=[],steps=[];
 if('stored'in spec)j.set('desktop:'+key,spec.stored);
 globalThis.desktopProbe={spawn(executable,args,options){
   calls.push({executable,arguments:args,options,timeoutMs:null,snapshot:snapshot(j)});
   const child=new EventEmitter();child.kill=()=>true;
   queueMicrotask(()=>{const event=spec.event??'success';if(event==='spawn-error'||event==='error-then-exit'){child.emit('error',Error('Synthetic launch failed'));if(event==='error-then-exit')child.emit('exit',0);}else if(event!=='timeout'){child.emit('exit',event==='nonzero'?1:event==='signal'?null:0);if(event==='exit-then-error')child.emit('error',Error('Late process error'));}});
   return child;
 }};
 globalThis.setTimeout=(fn,ms,...args)=>{if(ms!==30000)return nativeSet(fn,ms,...args);calls.at(-1).timeoutMs=ms;const timer={live:true};if(spec.event==='timeout')queueMicrotask(()=>{if(timer.live)fn();});return timer;};
 globalThis.clearTimeout=timer=>{if(timer&&Object.hasOwn(timer,'live'))timer.live=false;else nativeClear(timer);};
 for(const action of spec.actions){let result;try{result={value:await desktopCommand(action,key,j,translator(spec.language))};}catch(e){const x=IvyError.from(e);result={error:{code:x.code,message:x.message,outcome:x.outcome}};}steps.push({result,snapshot:snapshot(j)});}
 rows.push({spec,steps,calls});await j.close();
}}finally{Object.defineProperty(process,'platform',nativePlatform);globalThis.setTimeout=nativeSet;globalThis.clearTimeout=nativeClear;}
const output=JSON.stringify({rows,boundary:'Original desktop.ts and original SQLite journal execute unchanged. Process platform/spawn/timers are explicit probes; fixed script UTF-16/base64/argv, 30-second policy, cache/error order and all five logical tables are compared. No desktop activation or Host process acceptance.'});
if(process.env.IVY_CHAT_DESKTOP_ORACLE_CAPTURE)writeFileSync(process.env.IVY_CHAT_DESKTOP_ORACLE_CAPTURE,output);process.stdout.write(output);
