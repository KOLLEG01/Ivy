import assert from 'node:assert/strict';
import {WebSocketServer} from 'ws';
import {readFileSync,mkdirSync} from 'node:fs';
import {join} from 'node:path';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,settings,owner,rootId,at,itemId} from './inbox-fixtures.mjs';
import {snapshot,seedRetainedValues,seedTechnicalHistory} from './import-core-fixtures.mjs';
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {assignmentName,executionName,defaultConfiguration}=await load('services/secretary/src/assignment-schema.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {canonical,hashJson,deriveOperationId}=await load('packages/sdk/src/node.js');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js'),schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const sample=name=>JSON.parse(schemaSamples(ajv,[schema]).find(r=>r.valid&&r.reference.endsWith('#/$defs/'+name)).input);
const assignment={...sample('Assignment'),assignmentId:'retained-assignment',rules:{},enabled:true,trigger:{kind:'schedule',cadence:'interval',timeZone:'UTC',intervalMinutes:60,localTime:null,weekdays:[]},createdAt:at,updatedAt:at};
const assignmentId='00000000-0000-4000-8000-000000000003',archiveId='00000000-0000-4000-8000-000000000004',configurationId='00000000-0000-4000-8000-000000000099';
const archivedAssignment='archived\ud800';
const execution={...sample('Execution'),assignment:{objectId:assignmentId,revision:1},assignmentSnapshot:assignment};
execution.executionId=hashJson([assignment.assignmentId,execution.trigger.key]);execution.result=JSON.stringify({schemaVersion:1,urgency:'critical',notification:'voice',text:'retained notice',reason:'original'});delete execution.delivery;delete execution.voice;
const target={serviceNodeId:'fixture.agent',threadCwd:'/workspace',model:null,effort:'medium',permissions:':read-only'},configuration=defaultConfiguration(settings,at,target);
const catalog=JSON.parse(readFileSync('specs/native/codex-0.159.2/catalog.json','utf8'));
const native={serviceNodeId:'fixture.agent',hostId:'fixture-host',nativeVersion:catalog.version,nativeExecutableHash:catalog.nativeExecutableHash,catalogHash:hashJson(catalog)};
const status={...native,epoch:'native-epoch',pid:42,state:'ready',observedAt:at,code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
const resultHash=hashJson(execution.result),channel={adapter:'whatsapp',accountId:'fixture',channelId:'recipient'};
const main={executionId:execution.executionId,resultHash,operationId:'retained-main-operation',target:{serviceNodeId:'fixture.chat',channel},request:null,state:'queued',reason:'retained\ud800reason',evidence:null,expiresAt:null};
const voice={executionId:execution.executionId,resultHash,source:{objectId:itemId,revision:2},target:{serviceNodeId:'fixture.phone',recipientId:'recipient',requestDefinitionHash:'sha256:'+'a'.repeat(64)},request:{operationId:'retained-voice-operation',recipientId:'recipient',route:'voice',voicePrompt:'retained\udc00prompt'},callId:'retained-call',state:'outcome_unknown',reason:'original',expiresAt:null};
const location=process.argv[2];mkdirSync(location,{recursive:true});
const source=new SecretaryStore({},rootId,location);seedRetainedValues(source);
for(const [id,key,name,value] of [
 ['schedule','secretary/schedule-cursor',assignment.assignmentId,{updatedAt:at,checkedAt:at}],
 ['subscription','secretary/event-subscription',assignment.assignmentId,{name:'obsolete-active'}],
 ['archived-subscription','secretary/event-subscription',archivedAssignment,{name:'obsolete-archived'}],
 ['archived-schedule','secretary/schedule-cursor',archivedAssignment,{updatedAt:at,checkedAt:at}],
 ['archived-gap','secretary/event-gap',archivedAssignment,{prunedThroughSequence:0}],
 ['gap','secretary/event-gap',assignment.assignmentId,{prunedThroughSequence:0}],
 ['main','secretary/assignment-handoff',execution.executionId,main],
 ['voice','secretary/assignment-voice',execution.executionId,voice],
])seedTechnicalHistory(source,key,name,value,id);
const questions=[];
for(const [id,pending] of [['ready',false],['pending',true]]){
 const request={operationId:'fixture:1800000000000:'+id,itemId,question:'retained question\ud800'},name='follow-up-'+settings.identity.principalId+'-'+request.operationId;
 seedTechnicalHistory(source,'secretary/follow-up',name,{request,callerPrincipalId:settings.identity.principalId,threadId:null,turnId:null,phase:'prepared',calls:null,answer:null,errorCode:null,expiresAt:1800000000000},'question-'+id);
 const value={schemaVersion:1,itemId,threadId:pending?'':'retained-thread',target:native,threadCwd:'/workspace',developerInstructions:'retained instructions',retainedAt:at,expiresAt:1800000000000};
 seedTechnicalHistory(source,'secretary/follow-up-context',name,value,'context-'+id);
 questions.push({id:'question-'+id,contextId:'context-'+id,name,operationId:request.operationId,pending,startId:deriveOperationId(request.operationId,'start-secretary-follow-up-context')});
}
const checkpoint={action:'checkpoint',phase:'succeeded',checkpointCaptures:[{objectId:'retained-capture',revision:2}]};
source.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run('checkpoint','secretary/operation','checkpoint',2,0,1,canonical(checkpoint));
source.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run('checkpoint',1,'{ "retained":"\\ud800" }');source.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run('checkpoint',2,canonical(checkpoint));
const before=snapshot(source.db);source.close();
const bytes=readFileSync(join(location,'secretary.sqlite'));
const initial=[
 [assignmentId,{key:'secretary/assignment',name:assignmentName(assignment.assignmentId),versions:[{revision:1,value:assignment}]}],
 [archiveId,{key:'secretary/assignment',name:assignmentName(archivedAssignment),archived:true,versions:[{revision:1,value:{...assignment,assignmentId:archivedAssignment}}]}],
 [itemId,{key:'secretary/execution',name:executionName(assignment.assignmentId,execution.trigger.key),versions:[{revision:1,value:{...execution,phase:'queued'}},{revision:2,value:execution}]}],
 [configurationId,{key:'secretary/configuration',name:'configuration',versions:[{revision:1,value:configuration}]}],
];
let records,cached,expired,trace,options,epoch,counter,fired;
function reset(value={}){records=new Map(structuredClone(initial));cached=new Map();expired=new Set();trace=[];options=value;epoch=0;counter=20;fired=false;if(options.archivedExecution)records.get(itemId).archived=true;if(options.shadowedVoice)records.get(itemId).versions.at(-1).value.voice={...voice,request:{...voice.request,operationId:'current-voice-operation'}};if(options.missingExecution)records.delete(itemId);}
reset();
function raw(id,revision){
 const r=records.get(id);assert(r,'Missing fixture object '+id);const v=revision?r.versions.find(v=>v.revision===revision):r.versions.at(-1);assert(v);
 const request=source.objectWriteRequest(r.key,v.value,{object:{objectId:id,revision:v.revision}},'fixture:1800000000000:read');
 return{object:{id,parentId:rootId,effectivelyArchived:!!r.archived,contractKey:r.key},revision:{revision:v.revision,contractVersion:contractVersion(r.key),mediaType:'application/json',contentHash:hashJson(v.value),byteLength:Buffer.byteLength(canonical(v.value)),references:request.references,createdAt:at},content:{encoding:'json',value:structuredClone(v.value)}};
}
const field=(id,r,path)=>path==='object.parentId'?rootId:path==='object.name'?r.name:path==='object.effectivelyArchived'?!!r.archived:path==='object.archivedAt'?(r.archived?at:null):path.startsWith('data:/')?(r.versions.at(-1).value[path.slice(6)]??null):assert.fail(path);
const matches=(id,r,where)=>where.op==='and'?where.args.every(w=>matches(id,r,w)):where.op==='eq'?canonical(field(id,r,where.field))===canonical(where.value):assert.fail(where.op);
async function request(method,p){
 if(method==='fixture.reset'){reset(p);return{};}
 if(method==='fixture.resume'){epoch++;if(p.expire)for(const id of cached.keys())expired.add(id);return{};}
 if(method==='fixture.changeTarget'){const record=records.get(itemId),value=structuredClone(record.versions.at(-1).value);value.delivery.operationId='changed-original-operation';record.versions.push({revision:record.versions.length+1,value});return{};}
 if(method==='fixture.state')return{trace,records:[...records],cached:[...cached.keys()]};
 trace.push({method,params:structuredClone(p)});
 if(method==='service.connect')return{generation:7};if(method==='registry.sync')return{};if(method==='service.heartbeat')return{ready:false};
 if(method==='serviceNodes.get')return options.ownerChanged?{...owner,principalId:'foreign'}:owner;
 if(method==='system.status')return{runtimeEpoch:'fixture-'+epoch};
 if(method==='objects.read')return raw(p.objectId,p.revision);
 if(method==='objects.query'){
  const all=[...records].filter(([id,r])=>r.key===p.contractKey&&(p.includeArchived||!r.archived)&&matches(id,r,p.where)).sort(([a],[b])=>a.localeCompare(b));
  const start=p.cursor?Number(p.cursor.split(':')[1]):0,selected=all.slice(start,start+p.limit);
  return{items:selected.map(([id,r])=>({objectId:id,revision:r.versions.at(-1).revision,...(p.select?{values:Object.fromEntries(p.select.map(path=>[path,options.foreignQuery&&path==='object.parentId'&&r.key==='secretary/assignment'?'foreign':field(id,r,path)]))}:{})})),nextCursor:start+p.limit<all.length?'offset:'+(start+p.limit):null};
 }
 if(method==='objects.write'||method==='events.unsubscribe'){
  if(expired.has(p.mutationId))throw{code:'operation_expired',message:'The original fixture write receipt expired.',outcome:'unknown'};
  if(cached.has(p.mutationId))return structuredClone(cached.get(p.mutationId));
  const fault=!fired&&((method==='objects.write'&&options.fault==='write')||(method==='events.unsubscribe'&&options.fault==='unsubscribe'));
  if(fault){fired=true;if(!options.applied)throw{code:'fixture_unknown',message:'Original fixture reply unavailable.',outcome:'unknown'};}
  let result={};
  if(method==='objects.write'){
   let id=p.objectId;
   if(p.create){id='00000000-0000-4000-8000-'+String(counter++).padStart(12,'0');records.set(id,{key:p.create.contractKey,name:p.create.name,versions:[]});}
   const r=records.get(id);assert(r);const revision=r.versions.length+1;
   if(p.objectId&&p.expectedRevision!==r.versions.at(-1).revision)throw{code:'revision_conflict',message:'Original revision changed.',outcome:'not_executed'};
   r.versions.push({revision,value:structuredClone(p.content.value)});result={object:{id},revision:{revision}};
  }
  cached.set(p.mutationId,structuredClone(result));
  if(fault)throw{code:'fixture_unknown',message:'Original fixture reply unavailable.',outcome:'unknown'};
  return result;
 }
 if(method==='events.subscribe')return{items:[],throughSequence:0,gap:null,hasMore:false};
 if(method==='events.ack')return{};
 if(method==='events.read')return{items:[],gap:null,throughSequence:0,hasMore:false};
 if(method==='tools.list')return{provider:{node:{serviceNodeId:p.serviceNodeId}},items:[{qualifiedName:p.namespace+'.'+p.namePrefix,definition:{namespace:p.namespace,name:p.namePrefix,interfaceVersion:p.namespace==='codex'?native.nativeVersion:'1.0.0',inputSchema:{},outputSchema:{}},definitionHash:'sha256:'+'b'.repeat(64),catalogHash:native.catalogHash}],nextCursor:null};
 if(method==='tools.call'&&p.qualifiedName==='agent.status')return status;
 assert.fail('Import must not initialize a domain worker or dispatch '+method+' '+(p.qualifiedName??''));
}
const server=new WebSocketServer({host:'127.0.0.1',port:0});await new Promise((resolve,reject)=>{server.once('listening',resolve);server.once('error',reject);});
server.on('connection',socket=>socket.on('message',async bytes=>{const {id,method,params}=JSON.parse(bytes.toString());try{const result=await request(method,params);socket.send(JSON.stringify({jsonrpc:'2.0',id,result}));}catch(error){const code=error.code??'fixture_unexpected';socket.send(JSON.stringify({jsonrpc:'2.0',id,error:{code:-32000,message:error.message??'Fixture refused unexpected behavior.',data:{code,outcome:error.outcome??'unknown'}}}));}}));
process.stdout.write(JSON.stringify({port:server.address().port,settings,owner,rootId,coreBase64:bytes.toString('base64'),before,questions,main,voice,assignment,configuration,execution,at:Date.parse(at)+3600000})+'\n');
process.stdin.resume();process.stdin.on('end',()=>{for(const socket of server.clients)socket.terminate();server.close();});
