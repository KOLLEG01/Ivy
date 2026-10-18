import './chat-store-loader.mjs';
import {registerHooks} from 'node:module';
import {mkdtempSync,readFileSync,rmSync,writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {DatabaseSync} from 'node:sqlite';

// The monitor is the system under test. Its four existing Main boundaries are
// synthetic; use the real operation and SQLite journals on both sides.
const notices='data:text/javascript;base64,'+Buffer.from(`export class ChatNotices {
 constructor(main){this.main=main;} notify(...args){return this.main.notify(...args);}
 notice(...args){return this.main.notice(...args);}
}`).toString('base64');
const runtimeSdk='data:text/javascript;base64,'+Buffer.from([
 `export {hashJson} from '${new URL('../../../packages/contracts/src/canonical.ts',import.meta.url)}';`,
 `export {requireThat} from '${new URL('../../../packages/contracts/src/errors.ts',import.meta.url)}';`,
 `export {newOperationId} from '${new URL('../../../packages/sdk/src/client.ts',import.meta.url)}';`,
 `export {automaticNotificationsAllowed} from '${new URL('../../../packages/sdk/src/contact-window.ts',import.meta.url)}';`,
].join('\n')).toString('base64');
registerHooks({resolve(s,c,n){
 if(c.parentURL?.endsWith('/diagnostic-notifications.ts')){
  if(s==='./notices.js')return{url:notices,shortCircuit:true};
  if(s==='../../../packages/sdk/src/node.js')return{url:runtimeSdk,shortCircuit:true};
 }
 return n(s,c);
}});
const {DiagnosticNotifications}=await import('../../../services/chat-bridge/src/diagnostic-notifications.ts');
const {ChatStore}=await import('../../../services/chat-bridge/src/store.ts');
const {ChatAdmission}=await import('../../../services/chat-bridge/src/admission.ts');
const {ChatOperations}=await import('../../../services/chat-bridge/src/operations.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {hashJson}=await import('../../../packages/contracts/src/canonical.ts');
const {unpersistedNativePlan}=await import('../../fixtures/native-plan.ts');
const wall=Date.now(),now=Date.parse('2026-10-13T12:00:00Z'),delay=300000;
const channel={adapter:'whatsapp',accountId:'account',channelId:'channel'};
const definition={workspaceId:'chat',principalId:'chat-service',rootObjectId:null,
 project:{serviceNodeId:'owner',namespace:'codex',kind:'project',nativeId:'/synthetic/project'},
 nativePlan:unpersistedNativePlan(),channels:[{channel,displayName:'Channel'}]};
const admission=new ChatAdmission(definition),scope=hashJson([definition.principalId,definition.workspaceId]);
const pin={objectId:'conversation',revision:1};
const diagnostic=(code='tool_definition_changed',source='service:fixture.reader',resource=null,severity='warning')=>({code,source,resource,severity,status:'current',
 firstObservedAt:new Date(wall).toISOString(),lastObservedAt:new Date(wall).toISOString(),message:'Private request contents must not appear.'});
const base=diagnostic(),other=diagnostic('service_unavailable');
const page=(items=[],nextCursor=null)=>({items,nextCursor});
const tick=(items=[base],at=now,extra={})=>({action:'tick',now:at,pages:[page(items)],...extra});
const request=(name)=>({action:'notify',operationId:`chat:${wall}:seed-${name}`,source:pin,text:'Original retained wording '+name});
const pending=(name,queued=true,confirmed=false)=>({request:request(name),queued,confirmed});
const entry=(name='alert',extra={})=>({code:base.code,label:'fixture.reader',firstSeen:now-delay,active:true,resolvedAt:null,alert:pending(name),recovery:null,...extra});
const seed=(entries)=>Object.fromEntries(entries.map((e,i)=>['old-'+i,e]));
const cases=[];
const add=(label,steps,extra={})=>cases.push({label,language:'en',saved:{},steps,...extra});
const restart={action:'reopen'};
add('five-minute-alert-and-stable-recovery',[tick(),tick([base],now+delay-1),tick([base],now+delay),restart,
 tick([base],now+delay+1),tick([],now+delay+2),tick([],now+2*delay+1),tick([],now+2*delay+2),restart,tick([],now+2*delay+3)]);
add('german-wording-and-brief-recovery',[tick(),tick([base],now+delay),tick([],now+delay+1),restart,tick([base],now+delay+30000),tick([base],now+2*delay),tick([],now+2*delay+1),tick([],now+3*delay+1)],{language:'de'});
add('unknown-language-falls-back-to-english',[tick(),tick([base],now+delay)],{language:'DE'});
add('short-outages-reset-delay',[tick(),tick([base],now+150000),tick([],now+150001),tick([base],now+180000),tick([],now+330000),tick([base],now+360000),tick([base],now+660000)]);
add('complete-paginated-resource-group',[tick([base]),tick([],now+delay,{pages:[page([diagnostic(undefined,undefined,{objectId:'one'}),diagnostic(undefined,undefined,{objectId:'two'})],'next'),page([base])]}),
 tick([diagnostic(undefined,undefined,{objectId:'replacement'})],now+2*delay),tick([],now+2*delay+1),tick([],now+3*delay+1)]);
add('separate-callers-targets-and-codes',[tick([base,other,diagnostic(undefined,'ignored',{callerPrincipalId:'reader',serviceNodeId:'target'}),diagnostic(undefined,'ignored',{callerPrincipalId:'reader',serviceNodeId:'reader'})]),
 tick([base,other,diagnostic(undefined,'ignored',{callerPrincipalId:'reader',serviceNodeId:'target'}),diagnostic(undefined,'ignored',{callerPrincipalId:'reader',serviceNodeId:'reader'})],now+delay)]);
add('info-and-monitor-error-are-ignored',[tick([diagnostic(undefined,undefined,null,'info'),diagnostic('chat_diagnostics_unavailable',undefined,null,'error')]),tick([],now+delay)]);
add('unpaired-utf16-label-and-code',[tick([diagnostic('failure\ud800','service:reader\udfff')]),tick([diagnostic('failure\ud800','service:reader\udfff')],now+delay)]);
const quiet={timeZone:'Europe/Berlin',start:'23:00',end:'07:00'};
add('live-quiet-hours-discard-overnight-recovery',[
 tick([base],Date.parse('2026-10-13T21:00Z'),{quiet}),tick([base],Date.parse('2026-10-13T21:05Z'),{quiet}),tick([],Date.parse('2026-10-13T21:06Z'),{quiet}),
 tick([base],Date.parse('2026-10-14T04:50Z'),{quiet}),tick([base],Date.parse('2026-10-14T04:55Z'),{quiet}),
 tick([base],Date.parse('2026-10-14T05:00Z'),{quiet:{...quiet,end:'08:00'}}),restart,tick([base],Date.parse('2026-10-14T06:00Z'),{quiet})]);
add('quiet-hours-preserve-original-receipts',[tick([],Date.parse('2026-10-13T21:00Z'),{quiet}),restart,tick([],Date.parse('2026-10-14T06:00Z'),{quiet})],
 {saved:seed([entry('first',{active:false,resolvedAt:now-delay}),entry('receipt',{receiptOnly:true,active:false,resolvedAt:now-delay})])});
add('duplicate-group-retains-both-original-ids',[tick(),restart,tick(),tick([],now+1),tick([],now+delay+1)],
 {saved:seed([entry('one',{alert:pending('one',false)}),entry('two',{alert:pending('two',false)})])});
add('new-incident-retains-old-alert-and-recovery',[tick(),restart,tick([base],now+delay)],
 {saved:seed([entry('old-alert',{active:false,resolvedAt:now-delay,recovery:pending('old-recovery')})])});
add('recovery-created-but-not-queued-does-not-reopen',[tick(),tick([base],now+delay)],
 {saved:seed([entry('prior',{active:false,resolvedAt:now-delay,recovery:pending('prior-recovery',false)})])});
add('retained-receipt-missing-operation-is-not-retried',[tick()],{saved:seed([entry('missing')]),omitSeedOperations:true});
for(const phase of ['accepted','dispatching','outcome_unknown','succeeded'])add('review-'+phase,[tick(),restart,tick()],
 {saved:seed([entry('phase')]),seedPhase:phase,noticeState:phase==='succeeded'?'confirmed':'pending'});
add('delivered-reply-still-unconfirmed',[tick(),tick()],{saved:seed([entry('unconfirmed')]),seedPhase:'succeeded',noticeState:'pending'});
add('notice-publication-failure-keeps-pending-receipt',[tick([base],now,{noticeError:true}),restart,tick()],{saved:seed([entry('publication')]),seedPhase:'succeeded',noticeState:'confirmed'});
add('receipt-only-confirmation-removes-row',[tick(),restart,tick()],{saved:seed([entry('only',{receiptOnly:true,active:false,resolvedAt:now})]),seedPhase:'succeeded',noticeState:'confirmed'});
add('proven-unexecuted-failure-retries-with-new-id',[tick(),restart,tick()],{saved:seed([entry('failed')]),seedPhase:'failed',errorOutcome:'not_executed'});
add('unknown-failure-never-retries',[tick(),restart,tick()],{saved:seed([entry('unknown')]),seedPhase:'failed',errorOutcome:'unknown'});
add('native-readiness-failure-retains-original-id',[tick([base],now,{readyError:true}),restart,tick()],{saved:seed([entry('ready')]),seedPhase:'failed',errorOutcome:'not_executed'});
add('missing-main-keeps-incident-before-request',[tick(),tick([base],now+delay,{mainMissing:true}),tick([base],now+delay+1)]);
add('operation-id-read-failure-keeps-incident',[tick(),tick([base],now+delay,{failure:'status'}),restart,tick([base],now+delay+1)]);
for(const notifyError of ['before','after','returned-failed'])add('notification-'+notifyError,[tick(),tick([base],now+delay,{notifyError}),restart,tick([base],now+delay+1)]);
for(const failure of ['first-page','second-page','settings'])add('failed-'+failure+'-preserves-receipts',[tick([],now+delay,{pages:[page([],'next'),page([])],failure}),restart,tick()],{saved:seed([entry('snapshot')])});
for(const abort of ['before','page-0','page-1','settings','find','notify'])add('abort-'+abort,[tick([base,other],now+delay,{pages:[page([base],'next'),page([other])],abort}),restart,tick([base,other],now+delay+1)],{saved:seed([entry('aborted',{alert:null})])});
add('quiet-return-after-settings-abort',[tick([],Date.parse('2026-10-13T21:00Z'),{quiet,abort:'settings'})],{saved:seed([entry('quiet')])});
const completed=entry('completed',{active:false,resolvedAt:now,alert:pending('completed',true,true),recovery:pending('completed-recovery',true,true)});
add('completed-seven-day-boundary',[tick([],now+7*86400000),restart,tick([],now+7*86400000+1)],{saved:seed([completed])});
add('bounded-history-keeps-active-and-unconfirmed',[tick([],now)],{saved:seed([{...completed,code:'completed-oldest'},{...completed,code:'completed-newest',resolvedAt:now+1},entry('active',{code:'active'}),entry('unconfirmed',{code:'unconfirmed',active:false,resolvedAt:now})]),historyLimits:{count:3,bytes:16777216}});
add('confirmation-evicts-completed-row',[tick([]),restart,tick([])],{saved:seed([completed]),historyLimits:{count:0,bytes:1}});
add('review-confirmation-evicts-newly-completed-row',[tick([])],{saved:seed([{...completed,alert:pending('delivering'),recovery:pending('delivering-recovery')}]),seedPhase:'succeeded',noticeState:'confirmed',historyLimits:{count:0,bytes:1}});
add('lost-queued-marker-after-admission',[tick(),tick([base],now+delay),{action:'forgetQueued'},restart,tick([base],now+delay+1)]);

const all=[];
for(const c of cases){
 const root=mkdtempSync(join(tmpdir(),'ivy-chat-diagnostic-tick-'));
 const control={step:{},calls:[],page:0,signal:null};
 const error=()=>new IvyError('fixture_cancelled','The original monitor signal was aborted.','unknown');
 const log=(method,params={})=>control.calls.push({method,params:structuredClone(params)});
 const client={async request(method,params,options){
  log(method,params);if(options?.signal?.aborted)throw options.signal.reason;
  const s=control.step;
  if(method==='system.status'){if(s.failure==='status')throw new IvyError('fixture_unavailable','Operation identity read failed.');return{runtimeEpoch:'chat'};}
  if(method==='settings.get'){
   if(s.failure==='settings')throw new IvyError('fixture_unavailable','Settings read failed.');
   if(s.abort==='settings')control.signal.abort(error());
   return{settings:{schemaVersion:1,quietHours:s.quiet??null}};
  }
  if(method!=='system.diagnostics')throw Error('Unexpected RPC '+method);
  const index=control.page++;
  if(s.failure===(index===0?'first-page':'second-page'))throw new IvyError('fixture_unavailable','Diagnostic page failed.');
  if(s.abort==='page-'+index)control.signal.abort(error());
  return structuredClone(s.pages[index]);
 }};
 let store=new ChatStore(client,null,root),ops=new ChatOperations(store,admission);
 store.setRuntimeEpoch('chat');
 const admit=async(req)=>ops.begin(definition.principalId,{...req,expectedBridge:admission.expected(definition.principalId),channel});
 const patch=async(op,changes)=>store.write('chat-bridge/operation',{...op.value,...changes},'fixture-patch-'+op.pin.revision,{objectId:op.pin.objectId,expectedRevision:op.pin.revision});
 if(!c.omitSeedOperations)for(const e of Object.values(c.saved))for(const p of [e.alert,e.recovery])if(p){
  const op=await admit(p.request);await patch(op,{phase:c.seedPhase??'accepted',...(c.seedPhase==='failed'?{error:{code:'fixture_refused',detail:'Original refusal.',outcome:c.errorOutcome}}:{})});
 }
 store.saveDiagnosticNoticeState(scope,structuredClone(c.saved));
 if(c.historyLimits)Object.assign(store.diagnosticHistoryLimits,c.historyLimits);
 store.close();
 const db=new DatabaseSync(join(root,'chat-bridge.sqlite'));
 db.exec(`CREATE TABLE diagnostic_test_writes(value_json TEXT);
 CREATE TRIGGER diagnostic_test_insert AFTER INSERT ON markers WHEN NEW.key='diagnostic-notices:${scope}' BEGIN INSERT INTO diagnostic_test_writes VALUES(NEW.value_json); END;
 CREATE TRIGGER diagnostic_test_update AFTER UPDATE ON markers WHEN NEW.key='diagnostic-notices:${scope}' BEGIN INSERT INTO diagnostic_test_writes VALUES(NEW.value_json); END;`);
 db.close();
 c.database=readFileSync(join(root,'chat-bridge.sqlite')).toString('base64');
 store=new ChatStore(client,null,root);ops=new ChatOperations(store,admission);
 const main={get admission(){return admission;},get store(){return store;},get operations(){return ops;},
  async find(){log('main.find');if(control.step.abort==='find')control.signal.abort(error());return control.step.mainMissing?null:{value:{conversation:pin}};},
  native:{async ready(){log('main.ready');if(control.step.readyError)throw new IvyError('fixture_not_ready','Native owner is not ready.');}},
  async notify(caller,request){log('main.notify',{caller,request});const s=control.step;
   if(s.abort==='notify')control.signal.abort(error());
   if(s.notifyError==='before')throw new IvyError('fixture_unavailable','Notice admission failed.');
   const op=await admit(request);
   if(s.notifyError==='after')throw new IvyError('outcome_unknown','Notice reply was lost.','unknown');
   return s.notifyError==='returned-failed'?{phase:'failed'}:op.value;
  },
  async notice(caller,query){log('main.notice',{caller,query});if(control.step.noticeError)throw new IvyError('publication_pending','Original reply is not published yet.');return{data:{state:c.noticeState??'pending'}};}
 };
 c.expected=[];control.calls=[];let lastWrite=0;
 const snapshots=()=>{const d=new DatabaseSync(join(root,'chat-bridge.sqlite'));try{const rows=d.prepare('SELECT rowid,value_json FROM diagnostic_test_writes WHERE rowid>? ORDER BY rowid').all(lastWrite);lastWrite=rows.at(-1)?.rowid??lastWrite;return rows.map(r=>JSON.parse(r.value_json));}finally{d.close();}};
 try{
  for(const step of c.steps){
   control.step=step;control.page=0;control.calls=[];control.signal=new AbortController();
   let result={value:null};
   try{
    if(step.action==='reopen'){store.close();store=new ChatStore(client,null,root);ops=new ChatOperations(store,admission);if(c.historyLimits)Object.assign(store.diagnosticHistoryLimits,c.historyLimits);}
    else if(step.action==='forgetQueued'){const state=store.diagnosticNoticeState(scope);for(const e of Object.values(state))if(e.alert)e.alert.queued=false;store.saveDiagnosticNoticeState(scope,state);}
    else {if(step.abort==='before')control.signal.abort(error());await new DiagnosticNotifications(main,c.language,()=>step.now).tick(control.signal.signal);}
   }catch(error){const e=IvyError.from(error);result={error:{code:e.code,message:e.message,outcome:e.outcome}};}
   const d=new DatabaseSync(join(root,'chat-bridge.sqlite'));
   const operationCount=d.prepare("SELECT COUNT(*) AS count FROM documents WHERE key='chat-bridge/operation'").get().count;d.close();
   c.expected.push({result,calls:control.calls,writes:snapshots(),state:store.diagnosticNoticeState(scope),operationCount});
  }
 }finally{store.close();rmSync(root,{recursive:true,force:true});}
 // Explicit expectations guard against an oracle fixture accidentally becoming a
 // no-op or missing its intended interruption/pressure boundary.
 const notices=c.expected.flatMap(e=>e.calls).filter(c=>c.method==='main.notify');
 const final=c.expected.at(-1);
 if(c.label==='five-minute-alert-and-stable-recovery'&&(notices.length!==2||final.operationCount!==2))throw Error('Five-minute notice/recovery path was not exercised.');
 if(c.label==='duplicate-group-retains-both-original-ids'&&(notices.length!==3||final.operationCount!==3))throw Error('Grouped original receipts were not exercised.');
 if(c.label==='lost-queued-marker-after-admission'&&(notices.length!==2||notices[0].params.request.operationId!==notices[1].params.request.operationId||final.operationCount!==1))throw Error('Lost receipt did not reuse the original identity.');
 if(c.label==='bounded-history-keeps-active-and-unconfirmed'&&(!c.expected[0].writes.length||Object.values(final.state).some(e=>e.code==='completed-oldest')||Object.keys(final.state).length!==3))throw Error('Completed-history pressure was not forced.');
 if(c.label.includes('evicts-')&&Object.keys(final.state).length!==0)throw Error('Completed row was restored after eviction.');
 if(c.label==='completed-seven-day-boundary'&&(Object.keys(c.expected[0].state).length!==1||Object.keys(final.state).length!==0))throw Error('Seven-day deduplication boundary was not exercised.');
 for(const e of c.expected)if(e.result.error&&!['fixture_cancelled','fixture_not_ready','fixture_unavailable','publication_pending','outcome_unknown','chat_main_missing','chat_operational_notice_failed'].includes(e.result.error.code))throw Error('Unexpected fixture failure: '+c.label+' '+e.result.error.code);
 all.push(c);
}
process.stdout.write(JSON.stringify({definition,scope,cases:all}));
