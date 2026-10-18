import assert from 'node:assert/strict';
import {load,rootId,itemId,sourceId,at,id,caller,definition,source,message,policy,settings,owner,assessment,item} from './inbox-fixtures.mjs';
const {SecretaryEngine,noticeText,sourceName,identityHash,messageHash}=await load('services/secretary/src/engine.js');
const {SecretaryNotices}=await load('services/secretary/src/notices.js');
const {SecretaryStore,mutation}=await load('services/secretary/src/store.js');
const {defaultConfiguration}=await load('services/secretary/src/assignment-schema.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {IvyError,hashJson,canonical,parseOperationId}=await load('packages/sdk/src/node.js');
const clone=structuredClone,digest=c=>'sha256:'+c.repeat(64),rows=[];
const configId='00000000-0000-4000-8000-000000000005',replyId='00000000-0000-4000-8000-000000000006',inputId='00000000-0000-4000-8000-000000000007';
const messageTime=Date.parse(at),time=messageTime+300000,wall=1800000000000,operationId=id+'-assessment',retryKey='secretary/notice-retry';
const target={serviceNodeId:'fixture.chat',expectedBridge:{principalId:'chat-principal',callerPrincipalId:settings.identity.principalId,workspaceId:'workspace',definitionHash:digest('a')},channel:{adapter:'whatsapp',accountId:'account',channelId:'original'}};
const escalation={serviceNodeId:'fixture.phone',recipientId:'recipient',requestDefinitionHash:digest('a')};
Date.now=()=>wall;
async function add(label,c={}){
 const config=clone(c.settings??settings),snapshot=clone(c.policy??policy),assessed={...assessment,...c.assessment};
 const original={...message,...c.message};
 const decision={assessment:assessed,actor:caller,operationId,decidedAt:at,policy:snapshot,...c.decision};
 const pin={objectId:itemId,revision:c.prepared?3:2},text=noticeText(assessed,original,definition);
 const reason=c.suppressed??null;
 const notice={state:reason?'suppressed':'queued',reason,text,source:pin,target:null,request:null,preparedRevision:null,evidence:null,
  voice:assessed.notification==='voice'?{state:'queued',operationId:'00000000-0000-4000-8000-000000000008',callId:null,errorCode:null}:null,updatedAt:at};
 if(c.prepared){Object.assign(notice,{state:'dispatching',target:clone(target),request:{action:'notify',operationId:mutation(itemId,caller,operationId,'notice'),expectedBridge:clone(target.expectedBridge),channel:clone(target.channel),source:clone(pin),text:text+'\n\nSecretary reference: '+itemId},preparedRevision:3});}
 Object.assign(notice,clone(c.notice??{}));
 if(c.request)Object.assign(notice.request,c.request);
 if(c.voice)Object.assign(notice.voice,c.voice);
 const value={...clone(item),message:original,identityHash:identityHash('mail',original),messageHash:messageHash(original),decision,notice,...c.item};
 const input={label,settings:config,minimumAge:c.minimumAge??5,time:c.time??time,timeStep:c.timeStep??0,actions:c.actions??[{}],cancelled:c.cancelled??false,cancelAfter:c.cancelAfter??null,locals:[],technical:[],randomRetry:!!c.randomRetry};
 if(!c.noSource)input.locals.push({id:sourceId,key:'secretary/source',name:sourceName('mail'),value:{...source,...c.source}});
 if(c.retry)input.technical.push({id:'retry',key:retryKey,name:itemId,value:{failedOperationId:notice.request.operationId,attempts:1,nextAt:input.time+1,expiresAt:null,...c.retry}});
 const initialIssues=c.initialIssues??[['notice:'+itemId,'prior_issue']];input.initialIssues=initialIssues;
 const trace=[],steps=[],history=[],counts={},control=new AbortController();
 if(input.cancelled)control.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 let store,observed=input.time,clockCalls=0,currentRevision=c.revision??pin.revision,current=clone(value);
 const configuration={...defaultConfiguration(config,at),browserNotifications:c.browser??false};
 const raw=(objectId,value,key,revision=1)=>({object:{id:objectId,parentId:rootId,name:key==='secretary/item'?'item-'+value.identityHash.slice(7):'configuration',effectivelyArchived:false,contractKey:key},revision:{revision,contractVersion:contractVersion(key),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:store.objectWriteRequest(key,value,{object:{objectId,revision}},id).references,createdAt:at},content:{encoding:'json',value}});
 const proof=(key,value)=>({object:{contractKey:key,effectivelyArchived:false},revision:{contractVersion:'1.3.0',contentHash:hashJson(value)},content:{encoding:'json',value}});
 const reply=()=>({object:{objectId:replyId,revision:4},data:{origin:{kind:'native',inputId,binding:{threadId:'main-thread'},result:{turnId:'main-turn'}},channel:clone(target.channel),state:c.display??'confirmed',...c.reply}});
 const operation=()=>({callerPrincipalId:config.identity.principalId,request:clone(current.notice.request),phase:'succeeded',...c.operation});
 const forwarded=()=>clone(c.forwarded);
 const client={async request(method,p,options){
  const priorHistory=history.length;
  trace.push({method,params:clone(p),timeout:options?.timeoutMs??35000});
  const counter=method==='tools.call'?p.qualifiedName:method;counts[counter]=(counts[counter]??0)+1;
  try{
   if(trace.length===c.failAt||c.fault?.method===counter&&counts[counter]===(c.fault.n??1))throw new IvyError(c.failCode??c.fault?.code??'fixture_failure','Fixture failure.',c.outcome??c.fault?.outcome??'unknown');
   let result;
   if(method==='serviceNodes.get')result=p.serviceNodeId==='fixture.secretary'?{...owner,...c.owner}:p.serviceNodeId==='fixture.phone'?{serviceName:'phone-bridge',connected:true,synced:true,ready:true,...c.phoneNode}:{serviceNodeId:'fixture.chat',serviceName:'chat-bridge',principalId:'chat-principal',connected:true,synced:true,ready:true,...c.mainNode};
   else if(method==='settings.get')result={settings:{quietHours:c.globalQuiet??null}};
   else if(method==='system.status')result={runtimeEpoch:'fixture-epoch'};
   else if(method==='objects.query')result={items:[{objectId:configId,revision:1}],nextCursor:null};
   else if(method==='objects.read'){
    if(p.objectId===configId)result=raw(configId,configuration,'secretary/configuration');
    else if(p.objectId===itemId){if(c.missingItem)throw new IvyError('not_found','Fixture item missing.');result=raw(itemId,current,'secretary/item',currentRevision);}
    else if(p.objectId===replyId)result=proof('chat-bridge/reply',c.proofReply??reply().data);
    else if(p.objectId===inputId){const r=reply();result=proof('chat-bridge/input',{operationId:current.notice.request.operationId,identity:{senderPrincipalId:config.identity.principalId,channel:r.data.channel},binding:r.data.origin.binding,result:r.data.origin.result,...c.handoff});}
    else assert.fail('Unexpected object '+p.objectId);
   }else if(method==='objects.write'){
    if(c.writeCode&&!c.writeAfter)throw new IvyError(c.writeCode,'Fixture write failure.',c.outcome??'unknown');
    if(c.concurrentWrite||p.expectedRevision!==currentRevision)throw new IvyError('revision_conflict','Fixture revision changed.');
    current=clone(p.content.value);currentRevision++;history.push({revision:currentRevision,value:clone(current)});
    if(c.writeCode)throw new IvyError(c.writeCode,'Fixture write failure.',c.outcome??'unknown');
    result={object:{id:itemId},revision:{revision:currentRevision}};
   }else if(method==='events.publish')result={};
   else if(method==='tools.list'){
    const name=p.namespace+'.'+p.namePrefix;
    result={provider:{node:{serviceNodeId:p.serviceNodeId??'fixture.chat'}},items:c.noMain&&name==='chat.workspace'?[]:[{qualifiedName:name,definitionHash:c.changedPhoneHash&&name==='phone.request'?digest('b'):digest('a'),definition:{namespace:p.namespace,name:p.namePrefix,interfaceVersion:p.namespace==='chat'?'1.2.0':'1.0.0',annotations:{readOnlyHint:!['notify','request'].includes(p.namePrefix)},inputSchema:{},outputSchema:{}}}],nextCursor:null};
   }else if(method==='tools.call'){
    const args=p.arguments;
    if(p.qualifiedName==='chat.workspace')result={expectedBridge:clone(target.expectedBridge),nativeOwnerReady:true,channels:[{channel:clone(target.channel)}],...c.workspace};
    else if(p.qualifiedName==='chat.operation'){
     if(c.absent)throw new IvyError('not_found','Original operation absent.');
     result=operation();
    }else if(p.qualifiedName==='chat.notice'){
     if(c.pending)throw new IvyError('chat_publication_pending','Original publication pending.');
     result=c.nullReply?null:reply();
    }else if(p.qualifiedName==='chat.notify')result={...operation(),phase:c.phase??'accepted',error:c.error??null,outcome:c.phase==='succeeded'?{action:'notify',reply:c.nullReply?null:reply()}:null,...c.notify};
    else if(p.qualifiedName==='phone.request')result={callId:'call',operationId:notice.voice.operationId,recipientId:escalation.recipientId,principalId:config.identity.principalId,...c.admission};
    else if(p.qualifiedName==='phone.operation'){
     if(args.operationId)result=c.forwarded===null?null:forwarded();
     else result={phase:'replied',receipt:{ok:true,result:{state:args.method==='call.dial'?'connected':'sent'}},...c.phoneOperations?.[args.method]};
    }else assert.fail('Unexpected tool '+p.qualifiedName);
   }else assert.fail('Unexpected RPC '+method);
   if(input.cancelAfter===trace.length)control.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
   steps.push({method,value:clone(result),effect:history.length>priorHistory});return clone(result);
  }catch(cause){const e=IvyError.from(cause);steps.push({method,error:{code:e.code,message:e.message,outcome:e.outcome},effect:history.length>priorHistory});throw cause;}
 }};
 store=new SecretaryStore(client,rootId);
 for(const row of input.locals){const encoded=canonical(row.value);store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(row.id,row.key,row.name,1,0,null,encoded);store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(row.id,1,encoded);}
 for(const row of input.technical){const encoded=canonical(row.value);store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(row.id,row.key,row.name,1,encoded);store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(row.id,1,encoded);}
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings:config,owner:{serviceNodeId:'fixture.secretary'},epoch:Promise.resolve('fixture-epoch'),signal:control.signal,recoveryIssues:new Map(initialIssues),now:()=>new Date(observed+(clockCalls++)*input.timeStep)});
 const results=[];
 try{
  const notices=new SecretaryNotices(engine,input.minimumAge);
  for(const action of input.actions){if(action.time!==undefined)observed=action.time;try{await notices.step(itemId);results.push({value:null});}catch(cause){const e=IvyError.from(cause);results.push({code:e.code,message:e.message,outcome:e.outcome});}}
 }catch(cause){const e=IvyError.from(cause);results.push({code:e.code,message:e.message,outcome:e.outcome});}
 const technical=store.db.prepare('SELECT id,key,name,current_revision,value_json FROM technical_documents ORDER BY key,name').all().map(row=>({key:row.key,name:row.name,revision:row.current_revision,value:JSON.parse(row.value_json),history:store.db.prepare('SELECT revision,value_json FROM technical_revisions WHERE id=? ORDER BY revision').all(row.id).map(r=>({revision:r.revision,value:JSON.parse(r.value_json)}))}));
 const expected={results,trace,history,technical,issues:[...engine.recoveryIssues],clockCalls};
 // Only SDK-generated identities vary. Keep the request content and order exact,
 // validate every generated identity, and normalize the dependent write identity.
 let generated=current.notice?.request?.operationId;
 if(!c.randomRetry||generated===notice.request?.operationId)generated=null;
 if(generated)parseOperationId(generated);
 const walk=value=>{
  if(typeof value==='string')return value===generated?'<new-notice-operation>':value;
  if(Array.isArray(value))return value.map(walk);
  if(value&&typeof value==='object'){
   const next=Object.fromEntries(Object.entries(value).map(([k,v])=>[k,walk(v)]));
   if(value.method==='events.publish'){parseOperationId(value.params.mutationId);next.params.mutationId='<browser-operation>';}
   if(generated&&value.method==='objects.write'&&value.params.content.value.notice.request?.operationId===generated){parseOperationId(value.params.mutationId);next.params.mutationId='<retry-write-operation>';}
   return next;
  }return value;
 };
 store.close();input.steps=steps;rows.push({input,expected:walk(expected)});
 return rows.at(-1);
}
const valid=await add('prepare original notice then recover display',{actions:[{},{},{}]});
assert.equal(valid.expected.history.at(-1)?.value.notice.state,'confirmed',JSON.stringify(valid.expected));
await add('unassessed item',{item:{decision:null,notice:null}});
await add('missing item',{missingItem:true});
await add('missing source',{noSource:true});
await add('initial cancellation',{cancelled:true});
await add('changed Secretary owner',{owner:{synced:false}});
for(const minimumAge of [-1,0,0.5,5,1440,1441])await add('constructor age '+minimumAge,{minimumAge});
for(const offset of [299999,300000,300001])await add('message age boundary '+offset,{time:messageTime+offset});
for(const notification of ['none','needs_attention'])await add('suppressed '+notification,{assessment:{notification},suppressed:notification==='none'?'notification_none':'needs_attention'});
await add('suppression tampered dispatch',{assessment:{notification:'none'},suppressed:'notification_none',prepared:true});
await add('unexpected suppressed state',{notice:{state:'suppressed',reason:'record'}});
for(const options of [{settings:{...settings,sources:[]}},{settings:{...settings,sources:[{...definition,allowedSenderIds:[]}]}},{message:{outgoing:true}}])await add('authorization removed '+JSON.stringify(options),options);
const allNight={timeZone:'UTC',start:'23:00',end:'01:00'};
await add('global quiet hours',{globalQuiet:allNight});
await add('local quiet hours',{settings:{...settings,policy:{...policy,silentTime:allNight}}});
await add('critical local urgent bypass',{settings:{...settings,policy:{...policy,silentTime:allNight,urgentBypass:true}},assessment:{urgency:'critical'}});
await add('global quiet hours still cover local bypass',{globalQuiet:allNight,settings:{...settings,policy:{...policy,silentTime:allNight,urgentBypass:true}},assessment:{urgency:'critical'}});
await add('Main absent',{noMain:true});
await add('Main caller mismatch',{workspace:{expectedBridge:{principalId:'chat-principal',callerPrincipalId:'other'}}});
await add('quiet still discovers Main',{globalQuiet:allNight,noMain:true});
await add('browser published only once',{browser:true,timeStep:1000,actions:[{},{},{}]});
await add('unchanged reason makes no write',{notice:{reason:'minimum_message_age'},time:messageTime+1});
await add('unknown admission never replaced',{prepared:true,notice:{state:'outcome_unknown',reason:'lost_admission'}});
await add('late unknown display becomes confirmed',{prepared:true,notice:{state:'outcome_unknown',reason:'display_outcome_unknown'}});
await add('unknown display remains recoverable',{prepared:true,display:'outcome_unknown',actions:[{},{}]});
await add('pending display',{prepared:true,display:'queued',actions:[{},{}]});
await add('confirmed no provider replay',{prepared:true,notice:{state:'confirmed'},actions:[{},{}]});
for(const request of [{text:'changed'},{source:{objectId:itemId,revision:9}},{channel:{adapter:'whatsapp',channelId:'other'}},{expectedBridge:{principalId:'other',callerPrincipalId:'other'}}])await add('retained request mismatch '+JSON.stringify(request),{prepared:true,request});
for(const operation of [{callerPrincipalId:'other'},{request:{action:'notify',operationId:'other'}},{request:{action:'other'}},{request:{action:'notify',operationId:'other',source:{objectId:itemId,revision:2},text:'different'}}])await add('original Main identity mismatch '+JSON.stringify(operation),{prepared:true,operation});
// Metadata is adopted only when service content is exactly the same.
const originalRequest=valid.expected.history[0].value.notice.request;
await add('adopt original bridge metadata',{prepared:true,operation:{request:{...originalRequest,expectedBridge:{...target.expectedBridge,principalId:'new-principal'},channel:{...target.channel,channelId:'prior'}}}});
for(const phase of ['accepted','applying','succeeded','failed','unexpected'])await add('admission phase '+phase,{prepared:true,absent:true,phase,actions:[{},{}]});
await add('pending publication reuses original operation',{prepared:true,pending:true,actions:[{},{}]});
await add('bare legacy notice content',{prepared:true,request:{text:noticeText(assessment,message,definition)}});
for(const summary of ['Unpaired \ud800 é😀','\udc00','消息 🦉'])await add('lossless notice summary '+JSON.stringify(summary),{assessment:{summary},actions:[{},{}]});
await add('adopt admitted original metadata',{prepared:true,absent:true,notify:{request:{...originalRequest,expectedBridge:{...target.expectedBridge,workspaceId:'original-workspace'}}}});
await add('quiet recovery does not replay absence',{prepared:true,absent:true,globalQuiet:allNight});
await add('authorization removed still reads existing outcome',{prepared:true,settings:{...settings,sources:[]}});
await add('authorization removed refuses new admission',{prepared:true,absent:true,settings:{...settings,sources:[]}});
await add('empty original reply',{prepared:true,nullReply:true});
await add('rewritten reply proof',{prepared:true,proofReply:{changed:true}});
await add('wrong Main handoff',{prepared:true,handoff:{operationId:'other'}});
for(const outcome of ['not_executed','unknown','completed'])await add('terminal Main '+outcome,{prepared:true,absent:true,phase:'failed',error:{code:'provider_failed',outcome}});
for(const attempts of [0,1,3,4,5])await add('retry refused at attempt '+attempts,{prepared:true,absent:true,phase:'failed',error:{code:'provider_refused',outcome:'not_executed'},retry:{failedOperationId:'previous',attempts,nextAt:time-1}});
await add('backoff not elapsed',{prepared:true,retry:{nextAt:time+1}});
await add('retry cap before provider read',{prepared:true,retry:{attempts:5,nextAt:time}});
await add('new operation only after explicit failure',{prepared:true,retry:{nextAt:time},randomRetry:true});
await add('confirmed receipt clears old retry',{prepared:true,retry:{failedOperationId:'previous'}});
await add('admission unknown keeps original',{prepared:true,absent:true,fault:{method:'chat.notify',outcome:'unknown'},actions:[{},{}]});
await add('write conflicts propagate',{prepared:true,concurrentWrite:true});
for(const outcome of ['unknown','not_executed'])for(const writeAfter of [false,true])await add('lost save '+outcome+'/'+writeAfter,{prepared:true,writeCode:'outcome_unknown',writeAfter,outcome});
const critical={prepared:true,assessment:{notification:'voice',urgency:'critical'},policy:{...policy,voiceEscalation:escalation},notice:{state:'confirmed'},forwarded:null};
await add('Voice admits then reconciles dial and prompt',{...critical,actions:[{},{},{}],timeStep:1000});
await add('Voice already confirmed no replay',{...critical,voice:{state:'confirmed',callId:'call'}});
await add('Voice queued call awaits dial',{...critical,voice:{callId:'call'},phoneOperations:{'call.dial':{phase:'submitted'}}});
await add('Voice queued call awaits prompt',{...critical,voice:{callId:'call'},phoneOperations:{'call.promptVoice':{phase:'submitted'}}});
await add('Voice prompt uncertain',{...critical,voice:{callId:'call'},phoneOperations:{'call.promptVoice':{phase:'outcome_unknown'}}});
await add('Voice disconnected dial',{...critical,voice:{callId:'call'},phoneOperations:{'call.dial':{receipt:{ok:true,result:{state:'failed'}}}}});
await add('Voice global quiet prevents new call',{...critical,globalQuiet:allNight});
await add('Voice existing call reconciles during quiet',{...critical,voice:{callId:'call'},globalQuiet:allNight});
await add('Voice changed definition even for existing call',{...critical,voice:{callId:'call'},changedPhoneHash:true});
await add('Voice unavailable node',{...critical,phoneNode:{ready:false}});
await add('Voice admission changed recipient',{...critical,admission:{recipientId:'other'}});
await add('Voice admission missing call',{...critical,admission:{callId:null}});
await add('Voice admission unknown',{...critical,fault:{method:'phone.request',outcome:'unknown'}});
for(const code of ['revision_conflict','mutation_conflict'])await add('Voice write '+code,{...critical,writeCode:code});
await add('Voice requires critical snapshot',{...critical,assessment:{notification:'voice',urgency:'normal'}});
// Reuse the original forwarded prompt constructor rather than a second formatter.
const {voicePrompt}=await load('services/secretary/src/assignment-voice.js');
for(const state of ['submitted','result','outcome_unknown']){
 const intent={method:'call.forwardVoice',callId:'call',operationId:'00000000-0000-4000-8000-000000000008',prompt:voicePrompt(noticeText({...assessment,notification:'voice',urgency:'critical'},message,definition))};
 const forwarded={phase:state,intent,receipt:{ok:true,result:{state:'sent'}}};
 await add('Voice forwarded '+state,{...critical,voice:{callId:'call'},forwarded});
 await add('Voice forwarded admission '+state,{...critical,admission:{operationId:'other'},forwarded});
}
const recovered=await add('existing Main receipt confirmed',{prepared:true});
for(const outcome of ['not_executed','unknown','completed'])for(let failAt=1;failAt<=recovered.expected.trace.length;failAt++)await add('Main RPC boundary '+failAt+'/'+outcome,{prepared:true,failAt,outcome});
for(const outcome of ['not_executed','unknown','completed'])for(const failAt of [1,2,3,4,5,6,7,8,9,10,11])await add('Voice RPC boundary '+failAt+'/'+outcome,{...critical,voice:{callId:'call'},failAt,outcome});
for(const cancelAfter of [1,2,3,4,5,6,7,8])await add('cancel before next guard '+cancelAfter,{prepared:true,cancelAfter});
assert.ok(rows.length>=160);
assert.equal(rows.find(r=>r.input.label==='Voice admits then reconciles dial and prompt').expected.history.at(-1).value.notice.voice.state,'confirmed');
assert.equal(rows.find(r=>r.input.label==='new operation only after explicit failure').expected.history[0].value.notice.request.operationId,'<new-notice-operation>');
assert.equal(rows.find(r=>r.input.label==='retry refused at attempt 4').expected.history[0].value.notice.state,'outcome_unknown');
assert.ok(!rows.find(r=>r.input.label==='confirmed receipt clears old retry').expected.technical.some(r=>r.key===retryKey));
process.stdout.write(JSON.stringify({rootId,epoch:'fixture-epoch',rows}));
