import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,rootId,itemId,settings,owner,at} from './inbox-fixtures.mjs';
const {AssignmentVoice,voicePrompt}=await load('services/secretary/src/assignment-voice.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {IvyError,hashJson,canonical,parseOperationId}=await load('packages/sdk/src/node.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js');
const schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const seed=JSON.parse(schemaSamples(ajv,[schema]).find(r=>r.valid&&r.reference.endsWith('#/$defs/Execution')).input);
const key='secretary/execution',pin={objectId:itemId,revision:2},decision={text:'critical notice'};
const target={serviceNodeId:'fixture.phone',recipientId:'recipient',requestDefinitionHash:'sha256:'+'a'.repeat(64)};
const request={operationId:'original-voice',recipientId:target.recipientId,route:'voice',voicePrompt:voicePrompt(decision.text)};
const voice={executionId:seed.executionId,resultHash:hashJson(seed.result),source:pin,target,request,callId:'original-call',state:'dispatching',reason:null,expiresAt:null};
const sent={phase:'result',receipt:{ok:true,result:{state:'sent'}}},connected={phase:'result',receipt:{ok:true,result:{state:'connected'}}};
const forward={...sent,intent:{method:'call.forwardVoice',callId:voice.callId,operationId:request.operationId,prompt:request.voicePrompt}};
const rows=[];
async function add(label,options={}){
 const config=structuredClone(options),trace=[],steps=[],counts={};
 let current={...structuredClone(seed),voice:config.voice===null?null:{...structuredClone(voice),...config.voice}},revision=2;
 if(config.voice===null)delete current.voice;
 let store;
 const read=()=>({object:{id:itemId,parentId:rootId,effectivelyArchived:false,contractKey:key},revision:{revision,contractVersion:contractVersion(key),mediaType:'application/json',contentHash:hashJson(current),byteLength:Buffer.byteLength(canonical(current)),references:store.objectWriteRequest(key,current,{object:pin},'fixture:1800000000000:read').references,createdAt:at},content:{encoding:'json',value:structuredClone(current)}});
 const client={async request(method,params,opts){
  const copy=structuredClone(params);if(method==='objects.write'){parseOperationId(copy.mutationId);copy.mutationId='<scoped-operation>';}
  trace.push({method,params:copy,timeout:opts?.timeoutMs??35000});counts[method]=(counts[method]??0)+1;
  if(config.fail?.method===method&&counts[method]===(config.fail.n??1)){const error={code:config.fail.code??'fixture_failure',message:'Fixture failure.',outcome:config.fail.outcome??'unknown'};steps.push({method,error});throw new IvyError(error.code,error.message,error.outcome);}
  let value;
  if(method==='serviceNodes.get')value=params.serviceNodeId==='fixture.secretary'?{...owner,...config.owner}:{serviceName:'phone-bridge',connected:true,synced:true,ready:true};
  else if(method==='objects.read')value=read();
  else if(method==='objects.write'){assert.equal(params.expectedRevision,revision);current=structuredClone(params.content.value);revision++;value={object:{id:itemId},revision:{revision}};}
  else if(method==='system.status')value={runtimeEpoch:'fixture'};
  else if(method==='settings.get')value={settings:{quietHours:config.quiet?{timeZone:'UTC',start:'00:00',end:'23:59'}:null}};
  else if(method==='serviceNodes.list')value={items:[],nextCursor:null};
  else if(method==='tools.list')value={items:[{qualifiedName:'phone.'+params.namePrefix,definitionHash:target.requestDefinitionHash,definition:{interfaceVersion:'1.0.0',annotations:{readOnlyHint:params.namePrefix==='operation'}}}],provider:{node:{serviceNodeId:target.serviceNodeId}},nextCursor:null};
  else if(method==='tools.call'){
   if(params.qualifiedName==='phone.request')value={callId:'original-call',...request,principalId:settings.identity.principalId,...config.admission};
   else if(params.arguments.method==='call.dial')value='dial' in config?config.dial:connected;
   else if(params.arguments.method==='call.promptVoice')value='prompt' in config?config.prompt:sent;
   else value='forward' in config?config.forward:forward;
  }else throw Error('Unexpected '+method);
  steps.push({method,value:structuredClone(value)});return structuredClone(value);
 }};
 store=new SecretaryStore(client,rootId);
 if(config.legacy){const encoded=canonical(config.legacy);store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run('legacy','secretary/assignment-voice',seed.executionId,1,encoded);store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run('legacy',1,encoded);}
 const execution=store.decode(key,read());
 if(config.originalResult!==undefined)execution.value.result=config.originalResult;
 const controller=new AbortController();
 if(config.cancel)controller.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 const configured=config.fresh?{...settings,policy:{...settings.policy,voiceEscalation:target}}:settings;
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings:configured,owner:{serviceNodeId:'fixture.secretary'},signal:controller.signal,now:()=>new Date(at)});
 let result;
 try{const value=config.complete?await new AssignmentVoice(engine).completeFallback(execution):await new AssignmentVoice(engine).step(execution,decision,config.eligible??true);result={value:value??null};}
 catch(cause){const e=IvyError.from(cause);result={code:e.code,message:e.message,outcome:e.outcome};}
 finally{store.close();}
 rows.push({input:{label,execution,decision,settings:configured,generatedOperationId:config.fresh?trace.find(r=>r.method==='objects.write')?.params.content.value.voice.request.operationId:null,eligible:config.eligible??true,complete:config.complete??false,cancel:config.cancel??false,legacy:config.legacy??null,steps},expected:{result,trace,remaining:0}});
}
await add('forwarded-confirmed');await add('forwarded-pending',{forward:{...forward,phase:'submitted'}});
await add('forwarded-unknown',{forward:{...forward,phase:'outcome_unknown'}});
await add('forwarded-mismatch',{forward:{...forward,intent:{...forward.intent,operationId:'other'}}});
for(const state of ['confirmed','failed','fallback','outcome_unknown'])await add('saved-'+state,{voice:{state},eligible:false});
await add('ineligible',{voice:null,eligible:false});await add('no-target',{voice:null});
await add('quiet',{voice:{callId:null},quiet:true});await add('admission',{voice:{callId:null}});
await add('forwarded-admission',{voice:{callId:null},admission:{operationId:'other'}});
for(const field of ['recipientId','principalId','route','voicePrompt','callId'])await add('admission-'+field,{voice:{callId:null},admission:{[field]:field==='callId'?null:'other'}});
await add('dial-confirmed',{forward:null});
for(const dial of [null,{phase:'submitted'},{phase:'outcome_unknown'},{phase:'result',receipt:{ok:false}},{phase:'result',receipt:{ok:true,result:{state:'ringing'}}}])await add('dial-'+JSON.stringify(dial),{forward:null,dial});
for(const prompt of [null,{phase:'submitted'},{phase:'outcome_unknown'},{phase:'result',receipt:{ok:false}},{phase:'result',receipt:{ok:true,result:{state:'queued'}}}])await add('prompt-'+JSON.stringify(prompt),{forward:null,prompt});
for(const field of ['executionId','resultHash','source','request'])await add('saved-mismatch-'+field,{voice:{[field]:field==='source'?{...pin,objectId:rootId}:field==='request'?{...request,voicePrompt:'other'}:field==='resultHash'?'sha256:'+'0'.repeat(64):'other'}});
await add('result-changed',{originalResult:{changed:true}});
await add('owner-changed',{owner:{principalId:'other'}});await add('cancelled',{cancel:true});
for(const callId of [null,'original-call'])for(const outcome of ['not_executed','unknown','completed'])for(const code of ['fixture_failure','revision_conflict','mutation_conflict'])await add(`rpc-${callId}-${outcome}-${code}`,{voice:{callId},fail:{method:'tools.call',outcome,code}});
for(const code of ['fixture_failure','revision_conflict','mutation_conflict'])await add('save-'+code,{fail:{method:'objects.write',code,outcome:'unknown'}});
await add('unknown-repeat',{voice:{callId:null,state:'outcome_unknown',reason:'fixture_failure'},fail:{method:'tools.call',outcome:'unknown'}});
await add('legacy-migration',{voice:null,legacy:voice});
for(const state of ['failed','fallback','confirmed','dispatching'])for(const expiresAt of [null,1])await add(`complete-${state}-${expiresAt}`,{complete:true,voice:{state,expiresAt}});
await add('complete-absent',{complete:true,voice:null});
await add('fresh-admission-before-quiet',{fresh:true,quiet:true,voice:null});
await add('fresh-persistence-unknown',{fresh:true,voice:null,fail:{method:'objects.write',outcome:'unknown'}});
assert.ok(rows.length>=65);process.stdout.write(JSON.stringify({rootId,settings,now:Date.parse(at),rows}));
