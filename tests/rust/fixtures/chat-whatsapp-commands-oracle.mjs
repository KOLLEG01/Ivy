// Original command methods; only their existing owner/module bindings are probes.
import {readFileSync,writeFileSync} from 'node:fs';
import {stripTypeScriptTypes,registerHooks,syncBuiltinESMExports} from 'node:module';
import {fileURLToPath} from 'node:url';
import {existsSync} from 'node:fs';
import crypto from 'node:crypto';
const data=source=>'data:text/javascript;base64,'+Buffer.from(stripTypeScriptTypes(source,{mode:'transform'})).toString('base64');
const root=new URL('../../../',import.meta.url),url=p=>new URL(p,root).href;
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL?.startsWith('file:')){const target=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(target))return{url:target.href,shortCircuit:true};}return next(specifier,context);},load(target,context,next){if(target.startsWith('file:')&&target.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(fileURLToPath(target),'utf8'),{mode:'transform'}),shortCircuit:true};return next(target,context);}});
const errors=data(readFileSync(new URL('packages/contracts/src/errors.ts',root),'utf8'));
const {IvyError}=await import(errors);
const sdk=data(`export {IvyError,requireThat} from '${errors}';export {canonical,hashJson} from '${url('packages/contracts/src/canonical.ts')}';export function checkedNativeContract(){throw Error('Unexpected catalog boundary');}export function readNativeTurnContents(){throw Error('Unexpected contents boundary');}export function verifyNativeTurnContents(){throw Error('Unexpected verification boundary');}`);
const unused=data(`export class NativeOwner{constructor(){throw Error('Unexpected native construction');}}export class WhatsAppNative{constructor(){throw Error('Unexpected session construction');}}export function admittedNativeInput(){throw Error('Unexpected input boundary');}export function nativeThreadState(){throw Error('Unexpected state boundary');}export function mainTurnContext(){throw Error('Unexpected context boundary');}`);
const plan=data(`export async function resolveChatPlan(store){store.events.push(['plan']);return structuredClone(store.plan);}export function chatTurnOptions(){throw Error('Unexpected options boundary');}`);
const scoped=data(`export async function scopedOperationId(client,parts){client.events.push(['operationId',structuredClone(parts)]);return 'command-operation';}`);
const messages=data(readFileSync(new URL('services/chat-bridge/src/whatsapp/messages.ts',root),'utf8'));
const journal=data(readFileSync(new URL('services/chat-bridge/src/whatsapp/journal.ts',root),'utf8').replaceAll("'../../../../packages/sdk/src/node.js'",JSON.stringify(errors)));
const replacements={'../../../../packages/sdk/src/node.js':sdk,'../../../../packages/sdk/src/native-owner.js':unused,'./native.js':unused,'../native-plan.js':plan,'../input-media.js':unused,'./journal.js':journal,'../../../../packages/sdk/src/client.js':scoped,'../../../../packages/sdk/src/native-observations.js':unused,'./messages.js':messages,'../main-context.js':unused};
const load=async path=>{
 let source=readFileSync(new URL(path,root),'utf8');
 source=source.replace(/from\s+(["'])([^"']+)\1/g,(whole,quote,name)=>name in replacements?'from '+JSON.stringify(replacements[name]):whole);
 return import(data(source));
};
crypto.randomUUID=()=> '00000000-0000-4000-8000-000000000001';syncBuiltinESMExports();
Object.defineProperty(performance,'now',{value:()=>1000,configurable:true});
const {WhatsAppJournal,identity}=await import(journal),{translator}=await import(messages);
const {WhatsAppCommands,parseCommand}=await load('services/chat-bridge/src/whatsapp/commands.ts');
const {WhatsAppNative}=await load('services/chat-bridge/src/whatsapp/native.ts');
const clone=v=>structuredClone(v),capture=async f=>{try{return{value:await f()};}catch(error){const e=IvyError.from(error);return{error:{code:e.code,message:e.message,outcome:e.outcome}};}};
const snapshot=j=>Object.fromEntries(['kv','inbox','outgoing','retired_inbox','retired_outgoing'].map(table=>[table,j.db.prepare(`SELECT * FROM ${table} ORDER BY ${table==='kv'?'key':'id'}`).all().map(r=>Object.values(r))]));
const parsers=[];
for(const text of ['', '/', '//new','/New', '/new task','/STEER hello\nworld','\ufeff /help\u00a0','/help\u0085x','/mémoire','/hel1','/help\0','/help\ud800','/steer \ud800😀\udc00'])parsers.push({text,value:parseCommand(text)});
for(const space of ['\t','\n','\r','\v','\f',' ','\u00a0','\u1680','\u2000','\u2007','\u2028','\u2029','\u202f','\u205f','\u3000','\ufeff','\u0085','\u200b'])for(const name of ['new','StEeR','fast'])for(const text of [space+'/'+name+space,'/'+name+space+'one'+space+'two'+space])parsers.push({text,value:parseCommand(text)});
const main={pin:{objectId:'main',revision:7},value:{binding:{objectId:'binding',revision:3},queue:[]}},binding={pin:{objectId:'binding',revision:3},value:{primary:{nativeId:'thread'}}};
const basePlan={threadStart:{model:'plan-model'},turnStart:{model:'turn-model',effort:'high',serviceTier:'fast',collaborationMode:{mode:'plan',settings:{model:'mode-model',reasoning_effort:'low'}}}};
const models={data:[{id:'ID-A',model:'Model-A',displayName:'Model A',supportedReasoningEfforts:[{reasoningEffort:'high'},{reasoningEffort:'medium'}]},{id:'ID-B',model:'',displayName:'Second'}, {id:'ID-C',model:'Model-C',displayName:'Third',supportedReasoningEfforts:[]}],nextCursor:'ignored-original-page'};
const newResult={phase:'succeeded',outcome:{action:'createMain',binding:{object:{objectId:'new-binding',revision:1},data:{primary:{nativeId:'new-thread'}}}}};
const policySpecs=[];const add=(label,actions,patch={})=>policySpecs.push({label,actions:actions.map(text=>typeof text==='string'?{text}:text),...patch});
add('help-unknown',['/help','/start anything','/unknown','not a command','/new task','/task task']);
add('toggles',['/fast on','/commentary JA','/fast AUS','/commentary 0','/fast maybe','/commentary bad','/status']);
add('reasoning',['/reasoning HIGH','/reasoning reset','/reasoning default','/reasoning auto','/reasoning ultra','/reasoning invalid','/reasoning']);
add('models',['/models anything','/model id-a high','/model Model-A low','/model id-b medium','/model auto','/model reset high','/model default','/model missing','/model id-c high','/model id-a high extra','/model']);
add('thread-limits',['/threads','/tasks 25','/threads 0x10','/threads 1e1','/threads 1.0','/threads 0','/threads 26','/threads Infinity','/threads 1.5','/threads bad','/threads +2']);
add('desktop',['/desktop ensure','/desktop RESTART','/desktop']);
add('new-replay',[{text:'/new',id:'same'},{text:'/new',id:'same'}]);
add('new-pending',['/new'],{newResult:{phase:'accepted'}});
add('new-wrong-outcome',['/new'],{newResult:{phase:'succeeded',outcome:{action:'send'}}});
add('new-failed',['/new'],{newResult:{phase:'failed',error:{code:'original_failure'}}});
add('new-resumed-request',['/new'],{savedRequest:{action:'createMain',operationId:'old-operation',expectedMainRevision:2,reason:'old reason',expectedBridge:{original:true}}});
add('steer',['/steer hello\nworld']);add('steer-unicode',['/steer \ud800😀\udc00']);
add('steer-empty',['/steer']);add('steer-exact-limit',['/steer '+'😀'.repeat(65536)]);add('steer-too-long',['/steer '+'😀'.repeat(65537)]);
add('steer-missing',['/steer x'],{mainReads:[null]});add('steer-idle',['/steer x'],{read:{data:[{id:'turn',status:'completed'}]}});
add('steer-changed',['/steer x'],{mainReads:[main,{...main,value:{...main.value,binding:{objectId:'other',revision:8}}}]});
add('steer-revision-only',['/steer x'],{mainReads:[main,{...main,value:{...main.value,binding:{objectId:'binding',revision:4}}}]});
for(const prefs of [null,{model:null,effort:null,commentary:true,serviceTier:null,other:'keep'},{model:'chosen',effort:'max',serviceTier:'fast',commentary:true},['legacy'],false,'legacy'])add('preferences-'+JSON.stringify(prefs),['/status','/fast on','/reasoning reset','/status'],{preferences:prefs});
add('missing-main-status',['/status'],{mainReads:[null]});add('native-pending',['/models','/threads','/steer x'],{nativeResult:null});
add('models-malformed',['/models'],{models:{data:null}});
for(const language of ['de','fr'])add('language-'+language,['/help','/fast an','/reasoning max','/models','/threads','/new','/steer x','/unknown'],{language});
const policies=[];
for(const original of policySpecs){
 const spec=clone({mainReads:[main],binding,plan:basePlan,models,threads:{data:[{id:'one',name:'',preview:'unused'},{id:'two',name:null,preview:'😀'},{id:'three'}]},newResult,read:{data:[{id:'turn',status:'inProgress'}]},...original});
 const events=[],j=new WhatsAppJournal(':memory:');if('preferences'in spec)j.set('preferences',spec.preferences);let finds=0;
 const selected=()=>clone(spec.mainReads[Math.min(finds++,spec.mainReads.length-1)]);
 const store={events,plan:spec.plan,client:{events},async read(key,pin){events.push(['binding',key,clone(pin)]);return clone(spec.binding);}};
 const bridge={main:{store,admission:{expected(sender){events.push(['expected',sender]);return{sender};}},async find(){events.push(['main']);return selected();},async create(sender,request){events.push(['create',sender,clone(request)]);return clone(spec.newResult);}}};
 const native={invalidate(){events.push(['invalidate']);},async read(method,params){events.push(['read',method,clone(params)]);return clone(spec.read);},async command(key,method,factory,guard){events.push(['command',key,method]);const params=await factory();events.push(['params',clone(params)]);if(guard){events.push(['guard']);await guard();}events.push(['dispatch',snapshot(j)]);return 'nativeResult'in spec?clone(spec.nativeResult):clone(method==='model/list'?spec.models:method==='thread/list'?spec.threads:{ok:true});}};
 const commands=new WhatsAppCommands(bridge,j,native,async(action,key)=>{events.push(['desktop',action,key]);return'desktop-result';},translator(spec.language));
 const steps=[];
 for(let n=0;n<spec.actions.length;n++){
  const action=spec.actions[n],id=action.id??'row-'+n;
  j.accept({id,sender:'sender',jid:'fixture',message:id,received:n});if(spec.savedRequest)j.saveRequest(id,spec.savedRequest);
  const row=j.pending().find(r=>r.id===id);const result=await capture(()=>commands.run(row,action.text));
  steps.push({result,snapshot:snapshot(j),preferences:commands.preferences(),options:commands.turnOptions()});
 }
 policies.push({spec,steps,events});await j.close();
}
const dispatchSpecs=[];const addDispatch=(label,patch={})=>dispatchSpecs.push({label,key:'key',method:'turn/steer',params:{threadId:'thread',expectedTurnId:'turn',input:[],clientUserMessageId:'original-message'},actions:2,...patch});
addDispatch('fresh-replay');addDispatch('null-result',{reply:{result:null}});addDispatch('readonly-model',{method:'model/list',params:{limit:100}});addDispatch('readonly-thread',{method:'thread/list',params:{limit:10}});
for(const phase of ['accepted','dispatched','succeeded','failed','outcome_unknown'])addDispatch('legacy-'+phase,{legacy:{phase,...(phase==='succeeded'?{reply:{result:{old:true}}}:phase==='failed'?{reply:{error:{code:-1,message:'old failed'}}}:{})}});
addDispatch('legacy-absent',{legacy:{kind:'absent'}});addDispatch('legacy-unattempted',{legacy:{phase:'succeeded'},legacyAttempted:false});
addDispatch('recover-interaction',{attempted:true});addDispatch('method-conflict',{savedMethod:'turn/start'});addDispatch('stored-result',{attempted:true,savedResult:{retained:true}});
addDispatch('definite-refusal',{error:{code:'service_busy',message:'before execution',outcome:'not_executed'}});addDispatch('unknown-effect',{error:{code:'service_unavailable',message:'unknown effect',outcome:'unknown'}});
addDispatch('guard-refusal',{guardError:{code:'chat_binding_mismatch',message:'original Main changed',outcome:'not_executed'}});
addDispatch('native-error',{reply:{error:{code:-32600,message:'specific native rejection'}}});
for(const message of ['thread not loaded: thread','turn is already active','an active work item has a turn','turn\nis in progress','turn in progress','thread not loaded: other'])addDispatch('start-'+JSON.stringify(message),{method:'turn/start',reply:{error:{code:-32600,message}}});
addDispatch('start-success',{method:'turn/start'});addDispatch('start-busy-notification',{method:'turn/start',advanceBusy:true});
addDispatch('expired-found',{method:'turn/start',attempted:true,error:{code:'interaction_expired',message:'expired',outcome:'not_executed'},history:[{data:[{id:'recovered',items:[{type:'userMessage',clientId:'original-message'}]}],nextCursor:null}]});
addDispatch('expired-missing',{method:'turn/start',attempted:true,error:{code:'interaction_expired',message:'expired',outcome:'not_executed'},history:[{data:[],nextCursor:null}]});
addDispatch('expired-bounded',{method:'turn/start',attempted:true,error:{code:'interaction_expired',message:'expired',outcome:'not_executed'},history:[{data:[],nextCursor:'more'}],actions:1});
const dispatches=[];
for(const original of dispatchSpecs){
 const spec=clone(original),events=[],j=new WhatsAppJournal(':memory:'),hash='sha256:'+'a'.repeat(64);
 const call={operationId:'wa-'+identity(spec.key,spec.savedMethod??spec.method,spec.params),method:spec.savedMethod??spec.method,params:spec.params,definitionHash:hash};
 if(spec.attempted||spec.savedMethod||'savedResult'in spec)j.set('interaction:'+spec.key,{call,attempted:!!spec.attempted,...('savedResult'in spec?{result:spec.savedResult}:{})});
 if(spec.legacy)j.set('native:'+spec.key,{call,attempted:spec.legacyAttempted??true});
 const fail=e=>{throw new IvyError(e.code,e.message,e.outcome);};let histories=0;
 const native=Object.assign(Object.create(WhatsAppNative.prototype),{journal:j,t:translator(),busy:false,busyRevision:0,lastDispatchMs:null,prepared:'prepared',current:'current',invalidate(){this.current=null;events.push(['invalidate']);},async turnPage(thread,cursor){events.push(['history',thread,cursor]);return clone(spec.history[Math.min(histories++,spec.history.length-1)]);}});
 native.owner={async binding(method){events.push(['binding',method]);return{definitionHash:hash};},async operation(call){events.push(['operation',clone(call)]);return clone(spec.legacy);},async interaction(id){events.push(['interaction',id]);if(spec.error)fail(spec.error);return{reply:clone(spec.reply??{result:{native:true}})};},async interact(call,guard){events.push(['interact',clone(call)]);await guard();events.push(['effect',snapshot(j)]);if(spec.advanceBusy)native.busyRevision++;if(spec.error)fail(spec.error);return{reply:clone(spec.reply??{result:{native:true}})};}};
 const steps=[];for(let n=0;n<spec.actions;n++){
  const result=await capture(()=>native.command(spec.key,spec.method,async()=>{events.push(['parameters']);return clone(spec.params);},async()=>{events.push(['guard']);if(spec.guardError)fail(spec.guardError);}));
  steps.push({result,snapshot:snapshot(j),state:{busy:native.busy,revision:native.busyRevision,lastDispatchMs:native.lastDispatchMs,prepared:native.prepared}});
 }
 dispatches.push({spec,steps,events});await j.close();
}
const corpus={parsers,policies,dispatches,boundary:'Unchanged commands.ts and WhatsAppNative.command with actual original in-memory SQLite journal and translator. Main/plan/native session/desktop/owner network are explicit mandatory boundary probes; native owner adapter compiles against the real SDK. No account, complete WhatsAppNative runtime or installed/platform acceptance.'};
const output=JSON.stringify(corpus);if(process.env.IVY_CHAT_COMMANDS_ORACLE_CAPTURE)writeFileSync(process.env.IVY_CHAT_COMMANDS_ORACLE_CAPTURE,output);process.stdout.write(output);
