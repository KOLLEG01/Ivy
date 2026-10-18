import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,rootId,sourceId,at,caller,scope,definition,source,settings,owner} from './inbox-fixtures.mjs';
const {SecretaryEngine,sourceName}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {IvyError,canonical,hashJson}=await load('packages/sdk/src/node.js');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js');
const schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const sample=name=>JSON.parse(schemaSamples(ajv,[schema]).find(r=>r.valid&&r.reference.endsWith('#/$defs/'+name)).input);
const admitted={...settings,identity:{...settings.identity,serviceNodeId:'fixture.secretary'}};
const assignment={...sample('Assignment'),assignmentId:'assignment',name:'Original é😀',preflight:null};
const assignmentId='00000000-0000-4000-8000-000000000005';
const local=value=>({id:sourceId,key:'secretary/source',name:sourceName('mail'),value});
const bridge={principalId:'bridge-principal',callerPrincipalId:settings.identity.principalId};
const channel={adapter:'whatsapp',channelId:'original'};
const rows=[];
async function add(label,method,c={}){
 const config=structuredClone(c.settings??admitted),locals=c.locals??[local(source)];
 const input={label,method,args:c.args??[caller,{expectedScope:scope,assignmentId:'assignment'}],settings:config,locals,cancelled:c.cancelled??false,repeats:c.repeats??1,time:c.time??Date.parse(at)};
 const trace=[],epochTrace=[],steps=[];let owners=0,store;
 const raw=()=>{
  const value={...assignment,...c.assignment},revision=2;
  const read={object:{id:assignmentId,parentId:rootId,effectivelyArchived:false,contractKey:'secretary/assignment',...c.object},revision:{objectId:assignmentId,revision,contractVersion:contractVersion('secretary/assignment'),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:store.objectWriteRequest('secretary/assignment',value,{object:{objectId:assignmentId,revision}},'fixture:1800000000000:read').references,createdAt:at,...c.revision},content:{encoding:'json',value}};
  return read;
 };
 const client={async request(method,p,options){
  const entry={method,params:structuredClone(p),timeout:options?.timeoutMs??35000};
  // validateSettings qualifies constructor admission only. Its epoch lookup is
  // an independently owned generation input, retained separately for inspection.
  if(input.method==='validateSettings'){assert.equal(method,'system.status');epochTrace.push(entry);return {runtimeEpoch:'fixture'};}
  trace.push(entry);let value,error;
  if(c.fail===method)error={code:c.code??'fixture_failure',message:'Fixture failed.',outcome:'unknown'};
  else if(method==='serviceNodes.get'){
   if(p.serviceNodeId==='fixture.secretary'){owners++;value={...owner,...(owners===(c.ownerAt??1)?c.owner:{})};}
   else value=c.node===undefined?{serviceNodeId:'fixture.chat',principalId:bridge.principalId,serviceName:'chat-bridge',connected:true,synced:true,ready:true}:c.node;
  }else if(method==='objects.stat')value={effectivelyArchived:c.archived??false};
  else if(method==='system.status')value={runtimeEpoch:'fixture'};
  else if(method==='objects.query'){
   if(p.contractKey==='secretary/assignment'){
    const read=raw();value={items:c.missing?[]:[{objectId:assignmentId,revision:2,contractVersion:read.revision.contractVersion,document:read,...c.queryRow}],...(c.noCursor?{}:{nextCursor:c.cursor??null})};
   }else assert.fail('Unexpected query');
  }else if(method==='objects.read')value=raw();
  else if(method==='tools.list')value={provider:{node:{serviceNodeId:'fixture.chat'}},items:[{qualifiedName:'chat.workspace',definition:{namespace:'chat',name:'workspace',interfaceVersion:'1.2.0',inputSchema:{},outputSchema:{}},definitionHash:'sha256:'+'a'.repeat(64)}],nextCursor:null};
  else if(method==='tools.call')value=c.workspace===undefined?{expectedBridge:bridge,nativeOwnerReady:true,channels:[{channel}]}:c.workspace;
  else assert.fail('Unexpected RPC '+method);
  if(error){steps.push({method,error});throw new IvyError(error.code,error.message,error.outcome);}
  steps.push({method,value:structuredClone(value)});return structuredClone(value);
 }};
 store=new SecretaryStore(client,rootId);
 for(const row of locals){const encoded=canonical(row.value);store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(row.id,row.key,row.name,1,0,null,encoded);store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(row.id,1,encoded);}
 const abort=new AbortController();if(input.cancelled)abort.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings:config,owner:{serviceNodeId:'fixture.secretary'},signal:abort.signal,now:()=>new Date(input.time),recoveryIssues:new Map()});
 const results=[];
 for(let n=0;n<input.repeats;n++){
  try{
   if(method==='validateSettings'){
    const checked=new SecretaryEngine(client,config,{serviceNodeId:'fixture.secretary',generation:1},abort.signal);
    await checked.epoch;checked.store.close();results.push({value:null});
   }else results.push({value:(await engine[method](...input.args))??null});
  }catch(cause){const e=IvyError.from(cause);results.push({code:e.code,message:e.message,outcome:e.outcome});}
 }
 const retained=store.db.prepare('SELECT key,name,current_revision,value_json FROM documents ORDER BY key,name').all().map(r=>({key:r.key,name:r.name,revision:r.current_revision,value:JSON.parse(r.value_json)}));
 store.close();input.steps=steps;rows.push({input,expected:{results,trace,retained},epochTrace});
}
await add('valid settings','validateSettings');
for(const extra of [{identity:{...admitted.identity,hostId:''}},{pollMs:0},{recordsPerTick:0},{sources:[definition,definition]},{policy:{...settings.policy,researchMaxMinutes:6}},{policy:{...settings.policy,researchMaxMinutes:1.5}}])await add('invalid settings '+JSON.stringify(extra),'validateSettings',{settings:{...admitted,...extra}});
for(const zone of ['UTC','Europe/Berlin','America/New_York','+01:30','not-a-zone','\ud800'])await add('settings timezone '+zone,'validateSettings',{settings:{...admitted,policy:{...settings.policy,silentTime:{timeZone:zone,start:'22:00',end:'06:00'}}}});
await add('settings equal silent time','validateSettings',{settings:{...admitted,policy:{...settings.policy,silentTime:{timeZone:'UTC',start:'01:00',end:'01:00'}}}});
await add('initialize retained','initialize',{args:[],repeats:2});
await add('initialize new','initialize',{args:[],locals:[],repeats:2});
await add('initialize empty','initialize',{args:[],locals:[],settings:{...admitted,sources:[]}});
await add('initialize two sources','initialize',{args:[],locals:[],settings:{...admitted,sources:[definition,{...definition,sourceId:'second',accountId:'second-account'}]}});
await add('initialize policy refresh','initialize',{args:[],settings:{...admitted,sources:[{...definition,allowedSenderIds:['new'],producerPrincipalIds:['new']}]},locals:[local({...source,cursor:'retained-cursor',checkpointOperation:{principalId:'producer',operationId:'original'},updatedAt:at})],repeats:2});
for(const change of [{accountId:'other'},{kind:'teams'}])await add('initialize identity conflict','initialize',{args:[],settings:{...admitted,sources:[{...definition,...change}]}});
await add('initialize corrupted source','initialize',{args:[],locals:[local({...source,definitionHash:'sha256:'+'0'.repeat(64)})]});
await add('initialize archived','initialize',{args:[],archived:true});
await add('initialize cancelled','initialize',{args:[],cancelled:true});
await add('initialize owner mismatch','initialize',{args:[],owner:{principalId:'other'}});
await add('initialize refresh owner changed','initialize',{args:[],ownerAt:2,owner:{principalId:'other'},settings:{...admitted,sources:[{...definition,allowedSenderIds:[]}]}});
for(const fail of ['objects.stat','serviceNodes.get','system.status'])await add('initialize '+fail,'initialize',{args:[],fail,settings:{...admitted,sources:[{...definition,allowedSenderIds:[]}]}});
const list={expectedScope:scope};
await add('list default','listAssignments',{args:[caller,list]});
await add('list filters','listAssignments',{args:[caller,{...list,assignmentId:'assignment',enabled:false,triggerKind:'event',cursor:'next-page',limit:7}],cursor:'next-cursor'});
await add('list empty','listAssignments',{args:[caller,list],missing:true,noCursor:true});
for(const extra of [{limit:0},{limit:1000},{enabled:null},{triggerKind:'unknown'},{unexpected:true},{expectedScope:{...scope,rootObjectId:'other'}}])await add('list invalid '+JSON.stringify(extra),'listAssignments',{args:[caller,{...list,...extra}]});
await add('list cancelled','listAssignments',{args:[caller,list],cancelled:true});
await add('list owner mismatch','listAssignments',{args:[caller,list],owner:{hostId:'other'}});
await add('list wrong parent','listAssignments',{args:[caller,list],object:{parentId:'other'}});
await add('list corrupt content','listAssignments',{args:[caller,list],revision:{contentHash:'sha256:'+'0'.repeat(64)}});
await add('list missing document','listAssignments',{args:[caller,list],queryRow:{document:undefined}});
await add('get current','getAssignment');await add('get missing','getAssignment',{missing:true});
await add('get wrong id','getAssignment',{assignment:{assignmentId:'other'}});
await add('get archived','getAssignment',{object:{effectivelyArchived:true}});
await add('get invalid request','getAssignment',{args:[caller,{expectedScope:scope,assignmentId:''}]});
await add('get wrong scope','getAssignment',{args:[caller,{expectedScope:{...scope,secretaryId:'other'},assignmentId:'assignment'}]});
for(const fail of ['objects.query','objects.read','serviceNodes.get'])await add('get '+fail,'getAssignment',{fail});
for(const time of [-8640000000000001,-8640000000000000,8640000000000000,8640000000000001])for(const method of ['listAssignments','getAssignment'])await add('query timestamp '+time+' '+method,method,{time,args:method==='listAssignments'?[caller,list]:undefined});
await add('notice target','noticeTarget',{args:[]});
await add('notice caller mismatch','noticeTarget',{args:[],workspace:{expectedBridge:{...bridge,callerPrincipalId:'other'},nativeOwnerReady:true,channels:[{channel}]}});
for(const fail of ['tools.list','serviceNodes.get','tools.call'])await add('notice '+fail,'noticeTarget',{args:[],fail});
// Retained original probes for the separately requested checked Main catch API.
// They are not yet part of the Rust status qualification.
const statusProbes=[];
for(const [label,workspace,node,fail,code] of [['valid'],['coded failure',undefined,undefined,'tools.call','internal_error'],['null workspace',null],['null bridge',{expectedBridge:null,nativeOwnerReady:true,channels:[]}],['null channel',{expectedBridge:bridge,nativeOwnerReady:true,channels:[null]}],['null node',undefined,null]]){
 await add('status probe '+label,'status',{args:[caller,scope],settings:{...admitted,policy:{...settings.policy,voiceEscalation:{serviceNodeId:'fixture.phone',recipientId:'original',requestDefinitionHash:'sha256:'+'b'.repeat(64)}}},workspace,node,fail,code});statusProbes.push(rows.pop());
}
console.log(JSON.stringify({rootId,time:Date.parse(at),rows,statusProbes}));
