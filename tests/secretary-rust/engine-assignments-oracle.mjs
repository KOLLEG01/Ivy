import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,rootId,sourceId,operationId,at,caller,scope,source,settings,owner,accepted} from './inbox-fixtures.mjs';
const {SecretaryEngine,operationName,sourceName}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {IvyError,canonical,hashJson}=await load('packages/sdk/src/node.js');
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js');
const schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const sample=name=>JSON.parse(schemaSamples(ajv,[schema]).find(r=>r.valid&&r.reference.endsWith('#/$defs/'+name)).input);
const wall=1800000000000,id=`fixture-epoch:${wall}:engine`,key='secretary/public-operation';
Date.now=()=>wall;
const objectId='00000000-0000-4000-8000-000000000005',pin={objectId,revision:1};
const assignment={...sample('Assignment'),assignmentId:'assignment',name:'Original',preflight:null,rules:{main:{enabled:true},whatsapp:{enabled:false}},trigger:{kind:'schedule',timeZone:'UTC',cadence:'interval',intervalMinutes:60,localTime:null,weekdays:[]},createdAt:at,updatedAt:at};
const configuration={schemaVersion:1,rules:{main:{enabled:true,minimumUrgency:'normal',window:null},whatsapp:{enabled:false,minimumUrgency:'critical',window:null,questionsOnly:true},voice:{enabled:false,minimumUrgency:'critical',window:null,immediateOnly:true},researchMaxMinutes:0},execution:null,updatedAt:at};
const rows=[],templates={};
async function add(label,c={}){
 const method=c.method??'saveAssignment',kind=c.kind??(method==='saveConfiguration'?'configuration':'assignment');
 const base=kind==='configuration'?configuration:assignment;
 const request={operationId:id,...(kind==='configuration'?{configuration:pin}:{assignment:c.update?pin:null}),value:{...structuredClone(base),...c.value},...c.request};
 const input={label,method,kind,request,scope:c.scope??scope,caller:c.caller??caller,time:c.time??Date.parse(at),wall,expiredBefore:c.expiredBefore??0,repeats:c.repeats??1,settings,cancelled:c.cancelled??false,locals:[]};
 let current=c.missing?null:kind==='configuration'||c.update?structuredClone(base):null,revision=1,store,failed=false,written=false;
 const trace=[],steps=[];
 const raw=()=>{const value={...current,...c.current},contractKey='secretary/'+kind;return {object:{id:objectId,parentId:rootId,name:kind==='configuration'?'configuration':'unused',effectivelyArchived:false,contractKey},revision:{revision,contractVersion:contractVersion(contractKey),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:store.objectWriteRequest(contractKey,value,{object:{objectId,revision}},'fixture:1800000000000:read').references,createdAt:at},content:{encoding:'json',value}};};
 const client={async request(method,p,options){
  trace.push({method,params:structuredClone(p),timeout:options?.timeoutMs??35000});let value,error;
  if(c.fail===method)error={code:'fixture_failure',message:'Fixture failure.',outcome:c.outcome??'unknown'};
  else if(method==='serviceNodes.get')value={...owner,...c.owner};
  else if(method==='objects.query')value={items:current?[{objectId,revision}]:[],nextCursor:null};
  else if(method==='objects.read')value=raw();
  else if(method==='objects.writeReceipt'){
   value=c.receipt||written?{object:{id:objectId,parentId:rootId,contractKey:'secretary/'+kind,name:p.expectedRequestHash?'unused':undefined},revision:{revision:c.receipt?2:revision}}:null;
   if(value){const saved=store.technicalNamed(key,operationName(caller,id)).value;value.object.name=saved.write.create?.name??'unused';}
  }else if(method==='objects.write'){
   if(c.writeFailure&&!failed){failed=true;error={code:'fixture_write_failure',message:'Fixture write failed.',outcome:c.outcome??'unknown'};}
   if(!error||c.afterWrite){assert.equal(p.expectedRevision??1,revision);current=structuredClone(p.content.value);revision=p.create?1:revision+1;written=true;}
   value={object:{id:objectId},revision:{revision}};
  }else assert.fail('Unexpected RPC '+method);
  if(error){steps.push({method,error});throw new IvyError(error.code,error.message,error.outcome);}
  steps.push({method,value:structuredClone(value)});return structuredClone(value);
 }};
 store=new SecretaryStore(client,rootId);
 if(input.expiredBefore)store.db.prepare('INSERT INTO journal_state VALUES(?,?)').run('expiredBefore',String(input.expiredBefore));
 let saved=null;
 if(c.saved){
  saved={...structuredClone(templates[c.template??kind]),phase:c.savedPhase??'prepared',...c.savedChange};
  if(saved.phase!=='succeeded')delete saved.effect;
  store.technicalCreate(key,operationName(caller,id),saved);
 }
 input.saved=saved;
 if(c.inbox){
  const request={...structuredClone(accepted.request),operationId:id},first={...structuredClone(accepted),operationId:id,request,requestHash:hashJson(request)};
  const value=c.inbox==='accepted'?first:{...first,phase:'succeeded',effect:pin};
  input.locals=[{id:sourceId,key:'secretary/source',name:sourceName('mail'),value:source,first:source},{id:operationId,key:'secretary/operation',name:operationName(caller,id),value,first}];
  for(const row of input.locals){const rev=hashJson(row.first)===hashJson(row.value)?1:2;store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(row.id,row.key,row.name,rev,0,row.value.phase==='succeeded'?1:null,canonical(row.value));store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(row.id,1,canonical(row.first));if(rev===2)store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(row.id,2,canonical(row.value));}
 }
 const controller=new AbortController();if(input.cancelled)controller.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings,epoch:Promise.resolve('fixture-epoch'),owner:{serviceNodeId:'fixture.secretary'},signal:controller.signal,now:()=>new Date(input.time)});
 const results=[];
 for(let n=0;n<input.repeats;n++)try{results.push({value:await(method==='operationRead'?engine.operationRead(input.caller,input.scope,id):engine[method](input.caller,request))});}catch(e){e=IvyError.from(e);results.push({code:e.code,message:e.message,outcome:e.outcome});}
 const row=store.technicalNamed(key,operationName(caller,id));
 if(c.captureTemplate){assert.ok(row);templates[c.captureTemplate]=structuredClone(row.value);}
 const retained=row?{value:row.value,revision:row.pin.revision,cleanup:store.db.prepare('SELECT delete_after FROM technical_cleanup WHERE root_id=?').get(row.pin.objectId)?.delete_after??null}:null;
 store.close();input.steps=steps;rows.push({input,expected:{results,trace,retained}});
}
await add('fresh create',{captureTemplate:'assignment',repeats:2});
await add('fresh update',{update:true,captureTemplate:'update'});
await add('fresh configuration',{method:'saveConfiguration',captureTemplate:'configuration',repeats:2});
for(const method of ['saveAssignment','saveConfiguration']){
 const c={method};
 await add(method+' invalid request',{...c,request:{unexpected:true}});
 await add(method+' invalid value',{...c,value:{schemaVersion:2}});
 await add(method+' bad timezone',{...c,value:method==='saveAssignment'?{trigger:{...assignment.trigger,timeZone:'invalid'}}:{rules:{...configuration.rules,main:{...configuration.rules.main,window:{timeZone:'invalid',start:'01:00',end:'02:00'}}}}});
 await add(method+' cancelled',{...c,cancelled:true});
 await add(method+' owner changed',{...c,owner:{hostId:'other'}});
 await add(method+' expired new',{...c,expiredBefore:wall+1});
 await add(method+' wrong epoch',{...c,request:{operationId:`other:${wall}:engine`}});
 await add(method+' future operation',{...c,request:{operationId:`fixture-epoch:${wall+60001}:engine`}});
 await add(method+' invalid observation date',{...c,time:8640000000000001});
 await add(method+' inbox identity collision',{...c,inbox:'succeeded'});
 await add(method+' conflicting retained request',{...c,saved:true,value:method==='saveAssignment'?{name:'different'}:{browserNotifications:true}});
 await add(method+' retained failed',{...c,saved:true,savedPhase:'failed',savedChange:{errorCode:'prior_failure'}});
 await add(method+' retained success',{...c,saved:true,savedPhase:'succeeded'});
 await add(method+' retained expired receipt',{...c,saved:true,expiredBefore:wall+1,receipt:true});
 for(const outcome of ['not_executed','unknown','completed']){
  await add(method+' fresh write failure '+outcome,{...c,writeFailure:true,outcome,repeats:2});
  await add(method+' committed lost reply '+outcome,{...c,writeFailure:true,outcome,afterWrite:true,repeats:2});
  await add(method+' retained receipt failure '+outcome,{...c,saved:true,fail:'objects.writeReceipt',outcome});
 }
}
await add('create existing identity',{update:true,request:{assignment:null}});
await add('update missing',{update:true,missing:true});
await add('update stale pin',{update:true,request:{assignment:{...pin,revision:9}}});
await add('update changed creation time',{update:true,value:{createdAt:'2025-01-01T00:00:00.000Z'}});
await add('update changed identity',{update:true,current:{assignmentId:'other'}});
await add('configuration missing',{method:'saveConfiguration',missing:true});
await add('configuration stale pin',{method:'saveConfiguration',request:{configuration:{...pin,revision:9}}});
for(const fail of ['serviceNodes.get','objects.query','objects.read'])for(const method of ['saveAssignment','saveConfiguration'])await add(method+' RPC '+fail,{method,fail,update:method==='saveAssignment'});
await add('operation missing',{method:'operationRead'});
for(const inbox of ['accepted','succeeded'])await add('operation inbox '+inbox,{method:'operationRead',inbox});
await add('operation wrong scope',{method:'operationRead',scope:{...scope,secretaryId:'other'}});
await add('operation cancelled',{method:'operationRead',cancelled:true});
await add('operation wrong owner',{method:'operationRead',owner:{principalId:'other'}});
await add('operation invalid date before lookup',{method:'operationRead',time:8640000000000001});
for(const savedPhase of ['prepared','failed','succeeded'])for(const receipt of [false,true])await add('operation '+savedPhase+' receipt '+receipt,{method:'operationRead',saved:true,savedPhase,receipt,savedChange:savedPhase==='failed'?{errorCode:'prior_failure'}:{}});
for(const field of ['callerPrincipalId','operationId'])await add('operation retained identity '+field,{method:'operationRead',saved:true,savedChange:{[field]:'other'}});
for(const outcome of ['not_executed','unknown','completed'])await add('operation receipt failure '+outcome,{method:'operationRead',saved:true,fail:'objects.writeReceipt',outcome});
console.log(JSON.stringify({rootId,wall,epoch:'fixture-epoch',caller,id,rows}));
