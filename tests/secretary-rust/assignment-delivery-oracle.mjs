import assert from 'node:assert/strict';
import {importExecutionDelivery} from './execution-import-reference.mjs';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,rootId,itemId,settings,owner,at} from './inbox-fixtures.mjs';
const {AssignmentDelivery,assignmentDecision}=await load('services/secretary/src/assignment-delivery.js');
const {voicePrompt}=await load('services/secretary/src/assignment-voice.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {IvyError,hashJson,canonical,parseOperationId}=await load('packages/sdk/src/node.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js');
const schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const samples=schemaSamples(ajv,[schema]);
const seed=name=>JSON.parse(samples.find(r=>r.valid&&r.reference.endsWith('#/$defs/'+name)).input);
const baseDecision={schemaVersion:1,urgency:'high',notification:'main',text:'notice',reason:'useful'};
const decisions=[];
function parseCase(answer){let result;try{result={value:assignmentDecision(answer)};}catch(e){result={code:e.code,message:e.message,outcome:e.outcome};}decisions.push({answer,result});}
for(const value of [null,[],{},true,1,'string',baseDecision,{...baseDecision,extra:'ignored'}])parseCase(JSON.stringify(value));
for(const notification of ['none','main','voice','bad'])for(const urgency of ['low','normal','high','critical','bad'])parseCase(JSON.stringify({...baseDecision,notification,urgency}));
for(const field of ['schemaVersion','urgency','notification','text','reason'])for(const value of [null,{},[],false,2,''])parseCase(JSON.stringify({...baseDecision,[field]:value}));
for(const text of [' ','\ufeff','\u0085','\u200b','😀'.repeat(4096),'😀'.repeat(4097),'x'.repeat(8193),'\ud800','\udc00'])parseCase(JSON.stringify({...baseDecision,text}));
for(const reason of ['x'.repeat(1024),'x'.repeat(1025),'😀'.repeat(513)])parseCase(JSON.stringify({...baseDecision,reason}));
const json=JSON.stringify(baseDecision);
for(const prefix of ['```\n','```json\n','```JSON\n','```JsOn \t\r\n','```\ufeff\n','```\n \n','```yaml\n','```json ','```json\u2028'])for(const suffix of ['\n```','\r\n```','\n``` tail'])parseCase(prefix+json+suffix);
for(const text of ['', ' \ufeff'+json+'\ufeff ',json+'\n'+json,'{"text":"\ud800"}',json.replace('notice','\ud800'),json.replace('notice','\\\ud800')])parseCase(text);
const pin={objectId:itemId,revision:2},assignmentId='00000000-0000-4000-8000-000000000003',configurationId='00000000-0000-4000-8000-000000000004';
const replyPin={objectId:'00000000-0000-4000-8000-000000000005',revision:1},inputId='00000000-0000-4000-8000-000000000006';
const channel={adapter:'whatsapp',accountId:'fixture',channelId:'recipient'},bridge={principalId:'chat-principal',callerPrincipalId:settings.identity.principalId,workspaceId:'workspace',definitionHash:'sha256:'+'a'.repeat(64)};
const request={action:'notify',operationId:'original-main',expectedBridge:bridge,channel,source:pin,text:'original notice'};
const target={serviceNodeId:'fixture.chat',channel},origin={kind:'native',inputId,binding:{threadId:'thread'},result:{turnId:'turn'}};
const rules={main:{enabled:true,minimumUrgency:'normal',window:null},whatsapp:{enabled:true,minimumUrgency:'normal',window:null,questionsOnly:false},voice:{enabled:false,minimumUrgency:'critical',window:null,immediateOnly:true},researchMaxMinutes:0};
const rows=[];
async function add(label,config={}){
 const trace=[],steps=[],counts={},issues=new Map([['unrelated','retained']]);let store,revision=2,unexpected;
 const decision={...baseDecision,...config.decision};
 let current={...seed('Execution'),result:JSON.stringify(decision),effectiveRules:rules,...config.execution};
 const handoff={executionId:current.executionId,resultHash:hashJson(current.result),operationId:request.operationId,target,request,state:'dispatching',reason:null,evidence:null,expiresAt:null,...config.handoff};
 if(config.absent)delete current.delivery;else current.delivery=handoff;
 const assignment={...seed('Assignment'),enabled:true,rules:{},...config.assignment};
 const configuration={...seed('SecretaryConfiguration'),rules,browserNotifications:false,...config.configuration};
 const selected=new Map([[itemId,["secretary/execution",()=>current]],[assignmentId,["secretary/assignment",()=>assignment]],[configurationId,["secretary/configuration",()=>configuration]]]);
 const read=id=>{const [key,get]=selected.get(id),value=get(),rev=id===itemId?revision:1;return {object:{id,parentId:rootId,effectivelyArchived:false,contractKey:key},revision:{revision:rev,contractVersion:contractVersion(key),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:store.objectWriteRequest(key,value,{object:{objectId:id,revision:rev}},'fixture:1800000000000:read').references,createdAt:at},content:{encoding:'json',value:structuredClone(value)}};};
 const data={channel,origin,state:'confirmed',...config.reply};
 const input={operationId:request.operationId,identity:{senderPrincipalId:settings.identity.principalId,channel},binding:origin.binding,result:origin.result,...config.input};
 const document=(key,value,extra={})=>({object:{contractKey:key,effectivelyArchived:false,...extra.object},revision:{contractVersion:'1.3.0',contentHash:hashJson(value),...extra.revision},content:{encoding:'json',value,...extra.content}});
 const operation={request,callerPrincipalId:settings.identity.principalId,phase:'succeeded',...config.operation};
 const notify={...operation,outcome:{action:'notify',reply:{object:replyPin,data}},...config.notify};
 const client={async request(method,params,opts){
  const copied=structuredClone(params);if(method==='objects.write'){parseOperationId(copied.mutationId);copied.mutationId='<scoped-operation>';}
  trace.push({method,params:copied,timeout:opts?.timeoutMs??35000});counts[method]=(counts[method]??0)+1;
  let error;if(config.fail?.method===method&&(!config.fail.qualifiedName||config.fail.qualifiedName===params.qualifiedName)&&(!config.fail.n||counts[method]===config.fail.n))error={code:config.fail.code??'fixture_failure',outcome:config.fail.outcome??'unknown',message:'Fixture failure.'};
  if(method==='tools.call'&&params.qualifiedName==='chat.operation'&&config.missing)error={code:config.missing,outcome:'not_executed',message:'Fixture missing.'};
  if(error){steps.push({method,error});throw new IvyError(error.code,error.message,error.outcome);}
  let value;
  if(method==='serviceNodes.get')value=params.serviceNodeId==='fixture.secretary'?{...owner,...config.owner}:{serviceName:'chat-bridge',connected:true,synced:true,ready:true,...(config.fresh?{serviceNodeId:target.serviceNodeId,principalId:bridge.principalId}:{}),...config.provider};
  else if(method==='objects.query'){const id=params.contractKey==='secretary/assignment'?assignmentId:configurationId;value={items:config.missingRecord===params.contractKey?[]:[{objectId:id,revision:1}],nextCursor:null};}
  else if(method==='objects.read')value=params.objectId===replyPin.objectId?document('chat-bridge/reply',data,config.replyDocument):params.objectId===inputId?document('chat-bridge/input',config.fresh?{...input,operationId:current.delivery.request.operationId}:input,config.inputDocument):read(params.objectId);
  else if(method==='objects.write'){assert.equal(params.expectedRevision,revision);current=structuredClone(params.content.value);revision++;value={object:{id:itemId},revision:{revision}};}
  else if(method==='system.status')value={runtimeEpoch:'fixture'};
  else if(method==='settings.get')value={settings:{quietHours:config.quiet?{timeZone:'UTC',start:'00:00',end:'23:59'}:null}};
  else if(method==='tools.list')value={items:[{qualifiedName:'chat.'+params.namePrefix,definitionHash:bridge.definitionHash,definition:{interfaceVersion:'1.2.0',annotations:{readOnlyHint:params.namePrefix!=='notify'}}}],provider:{node:{serviceNodeId:target.serviceNodeId}},nextCursor:null};
  else if(method==='tools.call'){
   if(params.qualifiedName==='chat.workspace')value={expectedBridge:bridge,nativeOwnerReady:true,channels:[{channel}],...config.workspace};
   else if(params.qualifiedName==='chat.operation')value=config.fresh?{...operation,request:current.delivery.request}:operation;
   else if(params.qualifiedName==='chat.notify')value=config.fresh?{...notify,request:current.delivery.request}:notify;
   else value={object:replyPin,data};
  }
  else {unexpected=method;throw Error('Unexpected '+method);}
  steps.push({method,value:structuredClone(value)});return structuredClone(value);
 }};
 store=new SecretaryStore(client,rootId);
 let legacy=null;if(config.legacy){legacy=handoff;const encoded=canonical(legacy);store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run('legacy','secretary/assignment-handoff',current.executionId,1,encoded);store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run('legacy',1,encoded);delete current.delivery;}
 const execution=store.decode('secretary/execution',read(itemId));
 const controller=new AbortController();if(config.cancel)controller.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings,owner:{serviceNodeId:'fixture.secretary'},signal:controller.signal,now:()=>new Date(at),recoveryIssues:issues});
 let result;try{if(legacy)await importExecutionDelivery(engine,execution);result={value:await new AssignmentDelivery(engine,config.minimumAge??0).step(execution)};}catch(cause){const e=IvyError.from(cause);result={code:e.code,message:e.message,outcome:e.outcome};}finally{store.close();}
 assert.equal(unexpected,undefined,label);
 const generatedOperationId=config.fresh?trace.find(r=>r.method==='objects.write'&&r.params.content.value.delivery?.request)?.params.content.value.delivery.request.operationId??null:null;
 if(generatedOperationId)parseOperationId(generatedOperationId);
 // Fresh calls must use the identity already saved in the execution. Successful
 // and uncertain paths retain it; only a proven nonexecution may reset admission.
 if(config.fresh){let admitted=null;for(const entry of trace){if(entry.method==='objects.write'&&entry.params.content.value.delivery?.request)admitted=entry.params.content.value.delivery.request.operationId;if(entry.method==='tools.call'&&['chat.operation','chat.notify'].includes(entry.params.qualifiedName)){assert.ok(admitted,label);assert.equal(entry.params.arguments.operationId,admitted,label);if(entry.params.qualifiedName==='chat.notify')assert.equal(entry.params.operationId,admitted,label);}}}
 rows.push({input:{label,execution,steps,legacy,generatedOperationId,cancel:config.cancel??false,minimumAge:config.minimumAge??0},expected:{result,trace,remaining:0,issues:[...issues]}});
}
await add('confirmed');await add('pending',{reply:{state:'queued'}});await add('unknown',{reply:{state:'outcome_unknown'}});
for(const state of ['confirmed','suppressed','failed'])await add('saved-'+state,{handoff:{state}});
await add('retry-future',{handoff:{retryAt:Date.parse(at)+1}});
for(const phase of ['archived','settled'])await add('late-'+phase,{execution:{phase},handoff:{state:'outcome_unknown',evidence:replyPin}});
await add('terminal-missing',{missing:'not_found',handoff:{state:'outcome_unknown',terminal:true,evidence:replyPin}});
await add('terminal-no-proof',{handoff:{state:'outcome_unknown',terminal:true}});
for(const reason of ['not_found','chat_publication_pending'])await add('notify-'+reason,{missing:reason});
for(const phase of ['accepted','applying','bad'])await add('notify-'+phase,{missing:'not_found',notify:{phase,outcome:null}});
for(const attempts of [0,1,3,4,5])await add('retry-'+attempts,{missing:'not_found',handoff:{attempts},notify:{phase:'failed',error:{code:'fixture_failure',outcome:'not_executed'}}});
for(const outcome of ['unknown','completed'])await add('failed-'+outcome,{missing:'not_found',notify:{phase:'failed',error:{code:'fixture_failure',outcome}}});
await add('failed-without-error',{missing:'not_found',notify:{phase:'failed',error:null}});
for(const notification of ['none','main'])for(const absent of [true,false])await add(`quiet-${notification}-${absent}`,{decision:{notification},absent,quiet:true,missing:'not_found'});
for(const assignment of [{enabled:false},{rules:{main:{enabled:false}}},{rules:{main:{minimumUrgency:'critical'}}},{minimumNotificationAgeMinutes:60},{rules:{main:{window:{timeZone:'UTC',start:'01:00',end:'02:00'}}}}])await add('rule-'+JSON.stringify(assignment),{assignment,handoff:{request:null,operationId:null},execution:{trigger:{...seed('Execution').trigger,occurredAt:at}}});
await add('global-age',{minimumAge:60,handoff:{request:null,operationId:null},execution:{trigger:{...seed('Execution').trigger,occurredAt:at}}});
await add('migrated',{legacy:true});await add('fresh-suppressed',{absent:true,decision:{notification:'none'}});
for(const missingRecord of ['secretary/assignment','secretary/configuration'])await add('missing-'+missingRecord,{missingRecord});
for(const state of ['executionId','resultHash','operationId'])await add('identity-'+state,{handoff:{[state]:state==='resultHash'?'sha256:'+'0'.repeat(64):'other'}});
await add('source-mismatch',{handoff:{request:{...request,source:{...pin,objectId:rootId}}}});
await add('channel-mismatch',{handoff:{target:{...target,channel:{...channel,channelId:'other'}}}});
for(const changed of ['caller','action','text','transport']){const op=structuredClone(operationSeed());if(changed==='caller')op.callerPrincipalId='other';if(changed==='action')op.request.action='other';if(changed==='text')op.request.text='other';if(changed==='transport')op.request.channel={...channel,channelId:'new'};await add('operation-'+changed,{operation:op});await add('notify-'+changed,{missing:'not_found',notify:op});}
function operationSeed(){return {request:structuredClone(request),callerPrincipalId:settings.identity.principalId,phase:'succeeded'};}
await add('reply-channel',{reply:{channel:{...channel,channelId:'other'}}});await add('reply-origin',{reply:{origin:{...origin,kind:'manual'}}});
for(const which of ['replyDocument','inputDocument'])for(const extra of [{object:{effectivelyArchived:true}},{object:{contractKey:'other'}},{revision:{contractVersion:'1.2.0'}},{revision:{contractVersion:'9.0.0'}},{revision:{contentHash:'sha256:'+'0'.repeat(64)}},{content:{encoding:'base64'}}])await add(which+JSON.stringify(extra),{[which]:extra});
for(const extra of [{operationId:'other'},{identity:{senderPrincipalId:'other',channel}},{binding:{threadId:'other'}},{result:{turnId:'other'}}])await add('input-'+JSON.stringify(extra),{input:extra});
for(const key of ['serviceName','connected','synced','ready'])await add('provider-'+key,{provider:{[key]:key==='serviceName'?'other':false}});
await add('owner',{owner:{principalId:'other'}});await add('cancelled',{cancel:true});
for(const method of ['objects.write','tools.call','settings.get'])for(const outcome of ['not_executed','unknown','completed'])await add(method+outcome,{fail:{method,outcome,n:1}});
for(const state of ['confirmed','failed','dispatching']){
 const decision={...baseDecision,notification:'voice',urgency:'critical'};
 const voice={executionId:seed('Execution').executionId,resultHash:hashJson(JSON.stringify(decision)),source:pin,target:{serviceNodeId:'fixture.phone',recipientId:'recipient',requestDefinitionHash:bridge.definitionHash},request:{operationId:'original-voice',recipientId:'recipient',route:'voice',voicePrompt:voicePrompt(decision.text)},callId:null,state,reason:null,expiresAt:null};
 if(state==='dispatching')await add('voice-pending',{decision,execution:{voice},quiet:true});else await add('voice-'+state,{decision,execution:{voice}});
}
await add('fresh-confirmed',{fresh:true,absent:true,missing:'not_found'});
await add('fresh-original-found',{fresh:true,absent:true});
for(const phase of ['accepted','applying'])await add('fresh-'+phase,{fresh:true,absent:true,missing:'not_found',notify:{phase,outcome:null}});
for(const outcome of ['not_executed','unknown','completed'])await add('fresh-receipt-'+outcome,{fresh:true,absent:true,missing:'not_found',notify:{phase:'failed',error:{code:'fixture_failure',outcome}}});
for(const n of [1,2])for(const outcome of ['not_executed','unknown'])await add(`fresh-save-${n}-${outcome}`,{fresh:true,absent:true,fail:{method:'objects.write',n,outcome}});
for(const qualifiedName of ['chat.workspace','chat.operation','chat.notify'])for(const outcome of ['not_executed','unknown'])await add(`fresh-${qualifiedName}-${outcome}`,{fresh:true,absent:true,missing:qualifiedName==='chat.notify'?'not_found':undefined,fail:{method:'tools.call',qualifiedName,outcome}});
for(const workspace of [{nativeOwnerReady:false},{channels:[]},{channels:[{channel},{channel}]},{expectedBridge:{...bridge,callerPrincipalId:'other'}}])await add('fresh-workspace-'+JSON.stringify(workspace),{fresh:true,absent:true,workspace});
await add('fresh-provider-unavailable',{fresh:true,absent:true,provider:{ready:false}});
process.stdout.write(JSON.stringify({rootId,settings,now:Date.parse(at),decisions,rows}));
