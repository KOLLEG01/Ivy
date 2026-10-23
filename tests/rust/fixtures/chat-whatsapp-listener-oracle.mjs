// Unchanged listener/journal/messages methods; Main, native history, identity
// generation and cosmetic-name completion are explicit synthetic boundaries.
import {readFileSync,writeFileSync,existsSync} from 'node:fs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
const root=new URL('../../../',import.meta.url),url=p=>new URL(p,root).href;
const data=s=>'data:text/javascript;base64,'+Buffer.from(stripTypeScriptTypes(s,{mode:'transform'})).toString('base64');
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL?.startsWith('file:')){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return{url:u.href,shortCircuit:true};}return n(s,c);},load(u,c,n){if(u.startsWith('file:')&&u.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(fileURLToPath(u),'utf8'),{mode:'transform'}),shortCircuit:true};return n(u,c);}});
const errors=url('packages/contracts/src/errors.ts'),canonical=url('packages/contracts/src/canonical.ts');
const sdk=data(`export {IvyError,requireThat} from '${errors}';export {hashJson,canonical} from '${canonical}';`);
const journalModule=data(readFileSync(new URL('services/chat-bridge/src/whatsapp/journal.ts',root),'utf8').replaceAll("'../../../../packages/sdk/src/node.js'",JSON.stringify(sdk)));
const configModule=data(readFileSync(new URL('services/chat-bridge/src/whatsapp/config.ts',root),'utf8').replaceAll("'../../../../packages/sdk/src/node.js'",JSON.stringify(sdk)));
const scoped=data(`export async function scopedOperationId(client,scope){return client.operationId(scope);}`);
const originalStore=readFileSync(new URL('services/chat-bridge/src/store.ts',root),'utf8');
const mutationModule=data(`import {hashJson} from '${canonical}';`+originalStore.split('\n').find(line=>line.startsWith('export const mutation')));
let source=readFileSync(new URL('services/chat-bridge/src/whatsapp/listener.ts',root),'utf8');
for(const [from,to] of Object.entries({'../../../../packages/sdk/src/node.js':sdk,'./journal.js':journalModule,'./config.js':configModule,'../../../../packages/sdk/src/client.js':scoped,'./messages.js':url('services/chat-bridge/src/whatsapp/messages.ts'),'../store.js':mutationModule}))source=source.replaceAll(JSON.stringify(from),JSON.stringify(to)).replaceAll("'"+from+"'",JSON.stringify(to));
const {WhatsAppListener}=await import(data(source));
const {WhatsAppJournal}=await import(journalModule),{translator}=await import(url('services/chat-bridge/src/whatsapp/messages.ts')),{IvyError}=await import(errors),{canonical:canon,hashJson}=await import(canonical);
const clone=value=>structuredClone(value),capture=async work=>{try{return{value:await work()};}catch(error){const e=IvyError.from(error);return{error:{code:e.code,message:e.message,outcome:e.outcome}};}};
const config={accountId:'account',botPhoneNumber:'+15555550200',senders:[{phoneNumber:'+15555550201',principalId:'sender',channelId:'channel'}],since:'2024-01-01T00:00:00Z'};
const binding={pin:{objectId:'binding',revision:1},value:{primary:{nativeId:'thread'}}};
const user={id:'user',type:'userMessage',content:[]},agent={id:'answer',type:'agentMessage',text:'Answer'};
const turn=(id='turn',items=[user,agent],patch={})=>({id,status:'completed',itemsView:'full',items,...patch});
const page=(turns=[turn()],nextCursor=null)=>({data:turns,nextCursor});
const event=(method,params)=>({method,params:{threadId:'thread',...params}});
const notify=(item,extra={})=>({op:'notification',event:event('item/completed',{turnId:'turn',item}),...extra});
const tick=(commentary=false)=>({op:'tick',commentary});
const reply={object:{objectId:'reply',revision:1},data:{origin:{kind:'notice'},text:'Notice',channel:{adapter:'whatsapp',accountId:'account',channelId:'channel'}}};
const offer={object:{objectId:'offer',revision:1},data:{throughSequence:3}};
const received={phase:'succeeded',outcome:{action:'receive',offer,replies:[reply]}};
const nativeReply={...reply,data:{...reply.data,origin:{kind:'native',inputId:'input',result:{objectId:'result',revision:1},binding:{objectId:'binding',revision:1}}}};
const pending={request:{operationId:'retained-receive'},offer,replies:[reply]};
const spec=[],add=(label,actions,patch={})=>spec.push({label,actions,...patch});
add('fresh-main-idempotent',[tick(),tick(),{op:'mark'},tick()],{fresh:true});
add('existing-baseline-new-answer',[tick(),{op:'patch',value:{pages:[page([turn('turn',[user,agent,{...agent,id:'later',text:'Later'}])])]}},tick(),tick()]);
add('baseline-only-newest-page',[tick(),tick()],{pages:[page([turn()], 'older'),page([turn('old')])]});
add('active-last-item-not-complete',[tick(),{op:'patch',value:{pages:[page([turn('turn',[user,agent],{status:'completed'})])]}},tick()],{fresh:true,pages:[page([turn('turn',[user,agent],{status:'inProgress'})])]});
add('active-following-tool-fences-agent',[tick(),tick()],{fresh:true,pages:[page([turn('turn',[user,agent,{id:'tool',type:'commandExecution',output:'not delivered'}],{status:'inProgress'})])]});
for(const commentary of [false,true])add('commentary-'+commentary,[tick(commentary),tick(!commentary)],{fresh:true,pages:[page([turn('turn',[user,{...agent,phase:'commentary'},{...agent,id:'final',text:'Final'}])])]});
add('notification-then-history',[notify(agent),notify(user),notify(agent),tick(),{op:'mark'},tick()],{fresh:true});
add('notification-change-detected',[notify(user),notify(agent),notify({...agent,text:'Changed'}),tick()],{fresh:true});
add('history-change-detected',[tick(),{op:'patch',value:{pages:[page([turn('turn',[user,{...agent,text:'Changed'}])])]}},tick()],{fresh:true});
for(const method of ['item/agentMessage/delta','unrelated','turn/started','turn/completed'])add('notification-'+method,[{op:'notification',event:event(method,{})},{op:'notification',event:event(method,{threadId:'other'})}]);
for(const item of [null,[],0,{type:'tool'},{type:{toString:null}},{type:['agentMessage'],id:'x',text:'array type'}, {...agent,id:0},{...agent,text:null}])add('notification-filter-'+JSON.stringify(item),[notify(user),notify(item)]);
add('wrong-thread-no-source',[{op:'notification',event:event('item/completed',{threadId:'other',turnId:'turn',item:user})},notify(agent)]);
add('notice-input-not-mirrored',[notify({...user,clientId:'notice-input'}),notify(agent),tick()],{fresh:true,noticeInputs:['notice-input'],pages:[page([turn('turn',[{...user,clientId:'notice-input'},agent])])]});
add('notice-input-and-turn-UTF16-SQL-binding',[
 {op:'notification',event:event('item/completed',{turnId:'\ud800',item:{...user,clientId:'\udfff'}})},
 {op:'notification',event:event('item/completed',{turnId:'\ud800',item:agent})},tick(),
],{fresh:true,noticeInputs:['\udfff'],pages:[page([turn('\ud800',[{...user,clientId:'\udfff'},agent])])]});
add('UTF16-turn-and-body-retain-original-message-identity',[
 {op:'notification',event:event('item/completed',{turnId:'\ud800',item:user})},
 {op:'notification',event:event('item/completed',{turnId:'\ud800',item:{...agent,text:'Answer \udfff'}})},tick(),
],{fresh:true,pages:[page([turn('\ud800',[user,{...agent,text:'Answer \udfff'}])])]});
for(const value of [{status:'failed',error:{message:' model failed '}},{status:'failed',error:{message:'Refresh token has expired'}},{status:'interrupted'},{status:'completed'}])add('terminal-'+value.status+'-'+JSON.stringify(value.error),[tick(),tick(),{op:'mark'},tick()],{fresh:true,pages:[page([turn('turn',[],value)])]});
add('empty-agent-does-not-synthesize-terminal',[tick()],{fresh:true,pages:[page([turn('turn',[{...agent,text:'   '}])])]});
add('legacy-text-hash',[tick(),notify(user),notify(agent)],{fresh:true,kv:{'message:binding:turn:answer:channel':{text:'Answer',ids:[],suppressed:false}}});
add('same-key-enqueue-conflict-rolls-back-seen',[tick()],{fresh:true,initial:[{op:'seedOutput',key:'message:binding:turn:answer:channel',text:'Different'}]});
for(const pageValue of [{data:null,nextCursor:null},{data:[{...turn(),itemsView:'notLoaded'}],nextCursor:null},{data:[{...turn(),items:null}],nextCursor:null},{data:[turn()]},{data:[turn()],nextCursor:0}])add('malformed-page-'+JSON.stringify(pageValue),[tick()],{fresh:true,pages:[pageValue]});
add('cursor-must-progress',[tick(),tick()],{fresh:true,pages:[page([turn('one')],'same'),page([turn('two')],'same')]});
add('partial-64-page-recovery',[tick(),tick(),tick()],{fresh:true,pagedTurns:70});
add('time-bounded-recovery',[tick(),tick(),tick()],{fresh:true,pagedTurns:3,pageAdvance:12000});
add('restart-retains-old-watermark',[tick(),{op:'restart'},tick(),tick()],{fresh:true,pagedTurns:70});
add('binding-change-discards-partial',[tick(),{op:'patch',value:{binding:{...binding,pin:{objectId:'replacement',revision:1}},pagedTurns:1}},tick()],{fresh:true,pagedTurns:70});
add('native-page-error-retains-progress',[tick(),tick()],{fresh:true,pages:[page([turn('new')],'next'),{throw:{code:'native_read_pending',message:'retry original',outcome:'not_executed'}},page([turn('older')])]});
add('history-buffer-limit-clears-recovery',[tick(),{op:'patch',value:{wideHistory:false,pages:[page()]}},tick()],{fresh:true,wideHistory:true});
add('tool-payload-excluded-from-budget',[tick()],{fresh:true,wideTool:true});
for(const content of [[{codexDelegation:{sourceThreadId:'delegated-thread'}}],[{text:'<codex_delegation><source_thread_id> delegated-thread </source_thread_id></codex_delegation>'}]])add('delegated-output-'+JSON.stringify(content),[tick(),{op:'drain'}, {op:'patch',value:{pages:[page([turn('turn',[{...user,content},agent,{...agent,id:'later',text:'Later'}])])]}},tick()],{fresh:true,pages:[page([turn('turn',[{...user,content},agent])])],nameReply:{reply:{result:{thread:{name:'Named task'}}}}});
add('name-read-failure-is-cosmetic',[tick(),{op:'drain'},tick()],{fresh:true,pages:[page([turn('turn',[{...user,content:[{codexDelegation:{sourceThreadId:'delegated-thread'}}]},agent])])],nameReply:{throw:{code:'service_unavailable',message:'name unavailable',outcome:'not_executed'}}});
const sources=Array.from({length:257},(_,i)=>({op:'notification',event:event('item/completed',{turnId:'turn-'+i,item:{...user,content:[{codexDelegation:{sourceThreadId:'source-'+i}}]}})}));
add('source-name-caches-insertion-order',[...sources,{op:'drain'},{op:'notification',event:event('item/completed',{turnId:'turn-0',item:agent})},sources[0],{op:'drain'}]);
add('outbox-no-native-origin-advances-cursor',[{op:'outbox'},{op:'outbox'}],{history:{entries:[{data:{text:'user'}}],throughSequence:2}});
add('outbox-empty-history-no-regression',[{op:'outbox'}],{kv:{'reply-cursor:definition:channel':7},history:{entries:[],throughSequence:2}});
add('outbox-ordered-notice-ack',[{op:'outbox'},{op:'outbox'},{op:'mark'},{op:'outbox'}]);
for(const result of [{phase:'failed'},{phase:'accepted'},{phase:'succeeded',outcome:{action:'other'}}])add('receive-not-ready-'+JSON.stringify(result),[{op:'outbox'},{op:'outbox'}],{receives:[result,received]});
add('receive-error-keeps-request',[{op:'outbox'},{op:'outbox'}],{receives:[{throw:{code:'service_unavailable',message:'receive unavailable',outcome:'unknown'}},received]});
add('ack-error-keeps-original-ack',[{op:'outbox'},{op:'mark'},{op:'outbox'},{op:'outbox'}],{acknowledgements:[{throw:{code:'service_unavailable',message:'ack unknown',outcome:'unknown'}},{phase:'succeeded'}]});
add('ack-not-terminal-keeps-original-ack',[{op:'outbox'},{op:'mark'},{op:'outbox'},{op:'outbox'}],{acknowledgements:[{phase:'accepted'},{phase:'succeeded'}]});
add('expired-offer-reoffered-without-new-output',[{op:'outbox'},{op:'outbox'}],{kv:{'offer:definition:channel':pending},operations:[null],initial:[{op:'seedOutput',key:'notice:reply',text:'Notice',automatic:true,state:'sent'}]});
add('native-result-waits-for-original-output-ids',[{op:'outbox'},{op:'seedOutput',key:'late',text:'Late'},{op:'deliveryIds',key:'turn-delivered:binding:turn'},{op:'outbox'},{op:'mark'},{op:'outbox'}],{kv:{'offer:definition:channel':{...pending,replies:[nativeReply]}}});
add('native-already-delivered',[{op:'outbox'}],{kv:{'offer:definition:channel':{...pending,replies:[nativeReply]},'turn-delivered:binding:turn':true}});
add('canonical-notice-reply-must-be-confirmed',[{op:'outbox'},{op:'patch',value:{replyState:'confirmed'}},{op:'outbox'}],{kv:{'offer:definition:channel':{...pending,replies:[nativeReply]}},canonicalNotice:true});
add('native-result-read-failure-keeps-offer',[{op:'outbox'}],{kv:{'offer:definition:channel':{...pending,replies:[nativeReply]}},result:{throw:{code:'revision_pruned',message:'original result unavailable',outcome:'not_executed'}}});
add('empty-receive-advances-without-ack',[{op:'outbox'}],{receives:[{phase:'succeeded',outcome:{action:'receive',offer,replies:[]}}]});
const noticePart={pin:{objectId:'notice-part',revision:1},value:{text:'Pending notice'}};
add('pending-notice-confirmed-after-sent',[{op:'notices'},{op:'mark'},{op:'notices'},{op:'notices'}],{noticePages:[{replies:[{pin:{objectId:'notice',revision:1},value:{}}],nextCursor:'next'},{replies:[{pin:{objectId:'notice',revision:1},value:{}}],nextCursor:null},{replies:[],nextCursor:null}],noticeParts:[noticePart]});
add('pending-notice-reuses-mirrored-receipt',[{op:'notices'}],{noticePages:[{replies:[{pin:{objectId:'notice',revision:1},value:{}}],nextCursor:null}],noticeParts:[noticePart],initial:[{op:'seedOutput',key:'mirrored-notice:channel:notice-part',text:'Pending notice',automatic:true,state:'sent'}]});
add('pending-notice-confirm-failure-retains-cursor',[{op:'notices'},{op:'notices'}],{noticePages:[{replies:[{pin:{objectId:'notice',revision:1},value:{}}],nextCursor:'next'}],noticeParts:[noticePart],confirmations:[{throw:{code:'service_unavailable',message:'confirmation unavailable',outcome:'unknown'}},null],initial:[{op:'seedOutput',key:'notice:notice-part',text:'Pending notice',automatic:true,state:'sent'}]});
add('pending-notice-parts-must-all-be-sent',[{op:'notices'},{op:'mark',text:'Pending notice'},{op:'notices'},{op:'mark'},{op:'notices'}],{noticePages:[{replies:[{pin:{objectId:'notice',revision:1},value:{}}],nextCursor:null}],noticeParts:[noticePart,{pin:{objectId:'notice-part-2',revision:1},value:{text:'Second part'}}]});
add('retired-notice-id-remains-delivered',[{op:'notices'}],{noticePages:[{replies:[{pin:{objectId:'notice',revision:1},value:{}}],nextCursor:null}],noticeParts:[noticePart],initial:[{op:'seedOutput',key:'notice:notice-part',text:'Pending notice',automatic:true,state:'sent'},{op:'prune'}]});

const snapshot=j=>Object.fromEntries(['kv','inbox','outgoing','retired_inbox','retired_outgoing'].map(t=>[t,j.db.prepare(`SELECT * FROM ${t} ORDER BY ${t==='kv'?'key':'id'}`).all().map(r=>Object.values(r))]));
const rows=[];
for(const original of spec){
 const current=clone(original),j=new WhatsAppJournal(':memory:'),events=[],counts=new Map();let now=1000;
 Date.now=()=>now;
 const emit=(...args)=>events.push(clone(args));
 const unwrap=value=>{if(value&&typeof value==='object'&&value.throw)throw new IvyError(value.throw.code,value.throw.message,value.throw.outcome);return clone(value);};
 const take=(name,values)=>{const at=counts.get(name)??0;counts.set(name,at+1);return unwrap(values[Math.min(at,values.length-1)]);};
 const main={admission:{definitionHash:'definition',expected:sender=>({principalId:'bridge',callerPrincipalId:sender,workspaceId:'workspace',definitionHash:'definition'})},
  operations:{find:async(sender,id)=>{emit('operation',sender,id??null);return take('operations',current.operations??[{}]);}},
  store:{client:{operationId:async scope=>{emit('operationId',scope);return 'synthetic-operation:'+hashJson(scope).slice(7);}},
   isNoticeInput:id=>{emit('isNoticeInput',id);return (current.noticeInputs??[]).includes(id)||!!current.canonicalNotice;},
   read:async(contract,reference,parent)=>{emit('result',contract,reference,parent);return unwrap(current.result??{value:{turnId:'turn'}});}}};
 const outbox={
  history:async(sender,request)=>{emit('history',sender,request);return unwrap(current.history??{entries:[reply],throughSequence:3});},
  receive:async(sender,request)=>{emit('receive',sender,request);return take('receives',current.receives??[received]);},
  acknowledge:async(sender,request)=>{emit('acknowledge',sender,request);return take('acknowledgements',current.acknowledgements??[{phase:'succeeded'}]);},
  reply:async(id,channel)=>{emit('reply',id,channel);return {value:{state:current.replyState??'pending'}};},
  pendingNotices:async cursor=>{emit('pendingNotices',cursor??null);return take('noticePages',current.noticePages??[{replies:[],nextCursor:null}]);},
  noticeParts:async first=>{emit('noticeParts',first);return clone(current.noticeParts??[]);},
  confirmNotice:async parts=>{emit('confirmNotice',parts);return take('confirmations',current.confirmations??[null]);},
 };
 const native={bound:async()=>{emit('bound');return {binding:unwrap(current.binding??binding)};},
  turnPage:async(thread,cursor)=>{
   emit('turnPage',thread,cursor);now+=current.pageAdvance??0;
   if(current.wideHistory)return page([turn('wide-'+(cursor??0),[{...agent,text:'x'.repeat(17*1024*1024)}])],'next');
   if(current.wideTool)return page([turn('tool',[user,agent,{id:'tool',type:'commandExecution',output:'x'.repeat(2*1024*1024)}])]);
   if(current.pagedTurns){const offset=Number(cursor??0),id=current.pagedTurns-1-offset;return page([turn('turn-'+id,[{...agent,id:'item-'+id,text:'Answer '+id}])],offset+1<current.pagedTurns?String(offset+1):null);}
   return take('pages',current.pages??[page()]);
  },owner:{read:async(method,params,_options,force)=>{emit('sourceName',method,params,force);return unwrap(current.nameReply??{reply:{result:{thread:{name:'Resolved source'}}}});}}};
 const make=()=>new WhatsAppListener({main,outbox},j,native,current.config??config,translator(current.language??'en'));
 let listener=make();
 const apply=async action=>{
  switch(action.op){
   case'tick':return listener.tick(action.commentary??false);
   case'notification':return listener.notification(action.event,action.commentary??false);
   case'outbox':await listener.outbox();break;
   case'notices':await listener.notices();break;
   case'patch':Object.assign(current,clone(action.value));counts.clear();break;
   case'restart':listener=make();break;
   case'drain':await new Promise(resolve=>setImmediate(resolve));break;
   case'set':j.set(action.key,action.value);break;
   case'seedOutput':{const ids=j.enqueue(action.key,'15555550201@s.whatsapp.net',action.text,action.automatic??false);if(action.state)for(const id of ids)j.mark(id,action.state);break;}
   case'deliveryIds':j.set(action.key,j.outgoing().map(r=>r.id));break;
   case'mark':for(const row of j.outgoing())if(action.text===undefined||action.text===row.text)j.mark(row.id,action.state??'sent');break;
   case'prune':j.historyLimits={count:0,bytes:0};j.prune();break;
   default:throw Error('Unknown action '+action.op);
  }return null;
 };
 if(current.fresh)j.set('listener-new:binding',true);
 for(const [key,value]of Object.entries(current.kv??{}))j.set(key,value);
 for(const action of current.initial??[])await apply(action);
 events.length=0;
 const steps=[];
 for(const action of original.actions){const result=await capture(()=>apply(action));steps.push({result,events:events.splice(0),needsRecovery:listener.needsRecovery,snapshot:snapshot(j)});}
 await j.close();rows.push({label:original.label,steps});
}
const result={specs:spec,rows,identityBoundary:'Deterministic scoped-operation callback; real production adapter uses the shared SDK owner.'};
if(process.env.IVY_CHAT_LISTENER_ORACLE_CAPTURE)writeFileSync(process.env.IVY_CHAT_LISTENER_ORACLE_CAPTURE,JSON.stringify(result));
process.stdout.write(JSON.stringify(result));
