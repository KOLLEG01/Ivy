import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,settings,owner,rootId,at} from './inbox-fixtures.mjs';
const {AssignmentScheduler,selectDefaultAgentManager}=await load('services/secretary/src/assignment-scheduler.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {defaultConfiguration}=await load('services/secretary/src/assignment-schema.js');
const {IvyError,hashJson,canonical,parseOperationId}=await load('packages/sdk/src/node.js');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js'),schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const samples=schemaSamples(ajv,[schema]),sample=name=>JSON.parse(samples.find(r=>r.valid&&r.reference.endsWith('#/$defs/'+name)).input);
const configId='00000000-0000-4000-8000-000000000002',assignmentId='00000000-0000-4000-8000-000000000003',executionId='00000000-0000-4000-8000-000000000004';
const target={serviceNodeId:'fixture.agent',threadCwd:'/workspace',model:'base-model',effort:'high',permissions:':read-only'};
const node={serviceNodeId:'fixture.agent',serviceName:'agent-manager',hostId:settings.identity.hostId,connected:true,synced:true,ready:true,stale:false};
const selections=[];
for(const field of ['connected','synced','ready','stale'])for(const value of [false,true,undefined]){
 const nodes=[{...node,serviceNodeId:'remote',hostId:'remote'},{...node,[field]:value}];selections.push({nodes,hostId:settings.identity.hostId,expected:selectDefaultAgentManager(nodes,settings.identity.hostId)});
}
for(const nodes of [[],[{...node,serviceName:'other'}],[{...node,hostId:'remote'}],[node,{...node,serviceNodeId:'later'}],[{...node,hostId:'remote'},node],[{...node,hostId:'é😀'},node]])selections.push({nodes,hostId:settings.identity.hostId,expected:selectDefaultAgentManager(nodes,settings.identity.hostId)});
const rows=[];
async function add(label,c={}){
 const action=c.action??'defaultExecution',trace=[],steps=[],counts={},records=new Map();let store,listPage=0;
 const caseSettings={...settings,identity:{...settings.identity,scope:{...settings.identity.scope,secretaryId:c.secretaryId??settings.identity.scope.secretaryId}},policy:{...settings.policy,...c.policy}};
 const configuration={...defaultConfiguration(caseSettings,at,target),...c.configuration};
 if(c.legacy)configuration.rules.whatsapp={...configuration.rules.whatsapp,enabled:false,questionsOnly:true,minimumUrgency:'critical'};
 const assignment={pin:{objectId:assignmentId,revision:2},value:{...sample('Assignment'),rules:{},...c.assignment},writtenAt:at};
 const trigger={kind:'event',key:c.triggerKey??'original-trigger',occurredAt:at,payload:{kind:'fixture',text:'Original\ud800'}};
 if(c.presentConfig)records.set(configId,{key:'secretary/configuration',value:structuredClone(configuration),revision:1});
 if(c.presentExecution)records.set(executionId,{key:'secretary/execution',value:sample('Execution'),revision:1});
 const read=id=>{const r=records.get(id),value=r.value;return {object:{id,parentId:rootId,effectivelyArchived:false,contractKey:r.key},revision:{revision:r.revision,contractVersion:contractVersion(r.key),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:store.objectWriteRequest(r.key,value,{object:{objectId:id,revision:r.revision}},'fixture:1800000000000:read').references,createdAt:at},content:{encoding:'json',value:structuredClone(value)}};};
 const client={async request(method,p,options){
  const copied=structuredClone(p);if(method==='objects.write'){parseOperationId(copied.mutationId);copied.mutationId='<scoped-operation>';}
  trace.push({method,params:copied,timeout:options?.timeoutMs??35000});counts[method]=(counts[method]??0)+1;
  let result,error;const fault=c.faults?.find(f=>f.method===method&&f.n===counts[method]);
  if(c.fail===method||fault)error={code:fault?.code??'fixture_failure',message:'Fixture failure.',outcome:fault?.outcome??'unknown'};
  else if(method==='serviceNodes.get')result={...owner,...c.owner};
  else if(method==='serviceNodes.list')result=c.nodePages?.[listPage++]??{items:c.nodes??[node],nextCursor:null};
  else if(method==='system.status')result={runtimeEpoch:'fixture'};
  else if(method==='objects.query'){
   const id=p.contractKey==='secretary/configuration'?configId:executionId;
   result={items:records.has(id)?[{objectId:id,revision:records.get(id).revision}]:[],nextCursor:null};
  }else if(method==='objects.read')result=read(p.objectId);
  else if(method==='objects.write'){
   const key=p.create?.contractKey??records.get(p.objectId).key,id=p.create?(key==='secretary/configuration'?configId:executionId):p.objectId;
   const revision=p.create?1:records.get(id).revision+1;
   if(!p.create)assert.equal(p.expectedRevision,revision-1);
   records.set(id,{key,value:structuredClone(c.race?{...p.content.value,updatedAt:'2025-01-01T00:00:00.000Z'}:p.content.value),revision});
   if(c.race)error={code:'revision_conflict',message:'Fixture competing create.',outcome:'not_executed'};
   else result={object:{id},revision:{revision}};
  }else if(method==='tools.list')result={provider:{node:{serviceNodeId:p.serviceNodeId}},items:[{qualifiedName:'agent.resolveProject',definition:{namespace:'agent',name:'resolveProject',interfaceVersion:'1.0.0',inputSchema:{},outputSchema:{}},definitionHash:'sha256:'+'a'.repeat(64)}],nextCursor:null};
  else if(method==='tools.call'){assert.equal(p.qualifiedName,'agent.resolveProject');result=c.location??{cwd:'/workspace'};}
  else assert.fail('Unexpected RPC '+method);
  if(error){steps.push({method,error});throw new IvyError(error.code,error.message,error.outcome);}
  steps.push({method,value:structuredClone(result)});return result;
 }};
 store=new SecretaryStore(client,rootId);
 const controller=new AbortController();if(c.cancel)controller.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings:caseSettings,owner:{serviceNodeId:'fixture.secretary'},signal:controller.signal,now:()=>new Date(c.time??at)});
 const scheduler=new AssignmentScheduler(engine,c.fallback??null);let result;
 try{const out=action==='enqueue'?await scheduler.enqueue(assignment,trigger,configuration):await scheduler[action]();result={value:out??null};}
 catch(cause){const e=IvyError.from(cause);result={code:typeof cause.code==='string'?cause.code:e.code,message:typeof cause.message==='string'?cause.message:e.message,...(cause instanceof IvyError?{outcome:e.outcome}:{})};}
 const saved=[...records].map(([objectId,r])=>({objectId,key:r.key,revision:r.revision,value:r.value}));store.close();
 rows.push({input:{label,action,settings:caseSettings,configuration,assignment,trigger,fallback:c.fallback??null,steps,cancel:c.cancel??false,time:Date.parse(c.time??at)},expected:{result,trace},serverRecords:saved});
}
await add('default-ready');await add('default-no-manager',{nodes:[]});
await add('default-fallback',{fallback:target,nodes:[]});
await add('default-invalid-fallback',{fallback:{...target,threadCwd:'relative'}});
await add('default-prefer-local',{nodes:[{...node,serviceNodeId:'remote',hostId:'remote'},node]});
await add('default-pages',{nodePages:[{items:[{...node,serviceNodeId:'remote',hostId:'remote'}],nextCursor:'next'},{items:[node],nextCursor:''}]});
for(const location of [{},{cwd:'relative'},{cwd:'/workspace/é😀'}])await add('default-location-'+rows.length,{location});
for(const secretaryId of ['é😀','\ud800','\udc00'])await add('default-id-'+rows.length,{secretaryId});
for(const fail of ['serviceNodes.list','tools.list','tools.call'])await add('default-'+fail,{fail});
for(const c of [{},{nodes:[]},{fallback:target},{policy:{silentTime:{timeZone:'UTC',start:'22:00',end:'07:00'}}},{presentConfig:true},{presentConfig:true,legacy:true},{presentConfig:true,configuration:{execution:null}},{presentConfig:true,configuration:{execution:null},nodes:[]}])await add('initialize-'+rows.length,{action:'initialize',...c});
for(const c of [{},{presentConfig:true},{presentConfig:true,configuration:{execution:{...target,threadCwd:'relative'}}},{nodes:[]}])await add('configuration-'+rows.length,{action:'configuration',...c});
for(const action of ['initialize','configuration'])for(const fail of ['objects.query','objects.write','objects.read','system.status'])await add(action+'-'+fail,{action,fail});
await add('initialize-cancelled',{action:'initialize',cancel:true});
await add('initialize-owner-mismatch',{action:'initialize',owner:{principalId:'other'}});
await add('initialize-create-race',{action:'initialize',race:true});
for(const c of [{},{presentExecution:true},{configuration:{execution:null}},{assignment:{execution:{model:'override',effort:'low',reuse:'main'}}},{assignment:{execution:{model:null,effort:null,reuse:'new'}}},{assignment:{rules:{main:{enabled:false},researchMaxMinutes:7}}},{triggerKey:'é😀\ud800'},{assignment:{assignmentId:'assignment-\ud800'}},{race:true}])await add('enqueue-'+rows.length,{action:'enqueue',...c});
for(const fail of ['objects.query','objects.write','objects.read','system.status'])await add('enqueue-'+fail,{action:'enqueue',fail});
await add('enqueue-owner-is-outer-scope',{action:'enqueue',cancel:true});
await add('enqueue-valid-rule-overrides',{action:'enqueue',assignment:{rules:{main:{enabled:false},researchMaxMinutes:5}}});
await add('enqueue-existing-keeps-snapshot',{action:'enqueue',presentExecution:true,assignment:{prompt:'changed',execution:{model:'changed',effort:'low',reuse:'main'}}});
await add('initialize-unchanged-target-validation',{action:'initialize',presentConfig:true,configuration:{execution:{...target,threadCwd:'relative'}}});
await add('initialize-upgraded-target-validation',{action:'initialize',presentConfig:true,legacy:true,configuration:{execution:{...target,threadCwd:'relative'}}});
process.stdout.write(JSON.stringify({rootId,selections,rows}));
