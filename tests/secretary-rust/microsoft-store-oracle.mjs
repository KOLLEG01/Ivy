import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {load,rootId,settings,owner as serviceOwner,at} from './inbox-fixtures.mjs';
const {MicrosoftStore,MicrosoftEpochBoundaries}=await load('services/secretary/src/microsoft-store.js');
const {validateMicrosoft,microsoftBundled,microsoftRegistry,microsoftDefinitions,microsoftMethods}=await load('services/secretary/src/microsoft-schema.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {IvyError,canonical,hashJson,digest,NativeOwner}=await load('packages/sdk/src/node.js');
const clone=structuredClone,hash=d=>'sha256:'+d.repeat(64),pin={objectId:'plan',revision:1};
const target={serviceNodeId:'fixture.agent',hostId:'fixture-host',nativeVersion:'fixture-version',nativeExecutableHash:hash('a'),catalogHash:hash('b')};
const status={...target,epoch:'next-epoch',pid:42,state:'ready',observedAt:at,code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
const schemas=Object.fromEntries(['microsoft','teams'].map(k=>[k,JSON.parse(readFileSync('specs/schemas/secretary-'+k+'.schema.json','utf8'))]));
function seed(schema,doc){
 if(schema.$ref)return seed(doc.$defs[schema.$ref.split('/')[2]],doc);
 if('const'in schema)return schema.const;if(schema.enum)return schema.enum[0];
 if(schema.anyOf)return seed(schema.anyOf.find(s=>s.type==='null')??schema.anyOf[0],doc);
 if(schema.type==='object')return Object.fromEntries((schema.required??[]).map(k=>[k,seed(schema.properties[k],doc)]));
 if(schema.type==='array')return Array.from({length:schema.minItems??0},()=>seed(schema.items,doc));
 if(schema.type==='string')return schema.pattern?(schema.pattern.includes('sha256')?hash('c'):at):'v'.repeat(Math.max(1,schema.minLength??0));
 if(schema.type==='integer'||schema.type==='number')return schema.minimum??0;
 if(schema.type==='boolean')return false;if(schema.type==='null')return null;return{};
}
const values=Object.fromEntries(Object.entries(schemas).flatMap(([kind,doc])=>Object.entries(doc.$defs).filter(([n])=>kind==='microsoft'||n.startsWith('Teams')).map(([n,s])=>[n,seed(s,doc)])));
function result(e){const error=IvyError.from(e);return{code:error.code,outcome:error.outcome,...(error.code==='internal'?{}:{message:error.message})};}
const validation=[];function validate(label,name,value){let expected;try{validateMicrosoft(name,value);expected={value:null};}catch(e){expected=result(e);}validation.push({label,name,value,expected});}
const bundles=[];
for(const [name,value]of Object.entries(values)){
 validate(name+' valid',name,value);assert('value'in validation.at(-1).expected,name);
 bundles.push({name,hash:hashJson(microsoftBundled(name))});
 const doc=schemas[name.startsWith('Teams')?'teams':'microsoft'],schema=doc.$defs[name];
 for(const key of schema.required??[]){const invalid=clone(value);delete invalid[key];validate(name+' missing '+key,name,invalid);}
 validate(name+' extra',name,{...value,unexpected:true});
 for(const [key,s]of Object.entries(schema.properties??{})){
  if(s.type==='integer'){for(const n of [s.minimum-1,s.maximum+1,1.5].filter(Number.isFinite))validate(name+' range '+key+' '+n,name,{...value,[key]:n});}
  if(s.type==='string'&&s.maxLength)validate(name+' length '+key,name,{...value,[key]:'x'.repeat(s.maxLength+1)});
  if(s.pattern)validate(name+' pattern '+key,name,{...value,[key]:'not-valid'});
  if(s.type==='array'&&s.maxItems)validate(name+' array '+key,name,{...value,[key]:Array.from({length:s.maxItems+1},()=>seed(s.items,doc))});
 }
}
validate('calendar refinement remains in projection','OutlookSelection',{...values.OutlookSelection,since:'2026-02-29T00:00:00.000Z'});
validate('unknown Microsoft definition','NotPresent',{});validate('unknown Teams definition','TeamsNotPresent',{});
validate('default one-MiB frame','MicrosoftBinding',{...values.MicrosoftBinding,outputSchema:{description:'x'.repeat(1048576)}});
const operation=phase=>({schemaVersion:1,operationId:'original-operation',callerPrincipalId:settings.identity.principalId,...Object.fromEntries(['serviceNodeId','nativeVersion','nativeExecutableHash'].map(k=>[k,target[k]])),method:'account/read',params:{refreshToken:false},requestHash:hashJson({method:'account/read',params:{refreshToken:false}}),createdAt:at,updatedAt:at,phase,epoch:phase==='accepted'?null:'old-epoch',requestId:phase==='accepted'?null:42,reply:phase==='succeeded'?{result:{account:'original'}}:phase==='failed'?{error:{code:-1,message:'failed'}}:null,code:phase==='failed'?'native_error':phase==='outcome_unknown'?'outcome_unknown':null});
const originalCall={...values.OutlookCall,plan:pin,method:'account/read',params:{refreshToken:false},operationId:'original-operation',definitionHash:hash('d'),preparedAt:at};
const absence={kind:'agent_operation_absent',operationId:originalCall.operationId,serviceNodeId:target.serviceNodeId,epoch:status.epoch};
const rows=[];
const resolve=(value,results)=>typeof value==='string'&&/^\$\d+\./.test(value)?value.slice(1).split('.').reduce((v,k)=>v[k],results):Array.isArray(value)?value.map(v=>resolve(v,results)):value&&typeof value==='object'?Object.fromEntries(Object.entries(value).map(([k,v])=>[k,resolve(v,results)])):value;
async function add(label,actions,c={}){
 const input={label,actions,documents:[],blobs:[],...clone(c)},trace=[],steps=[];let guardCalls=0;
 const client={async request(method,p,options){trace.push({method,params:clone(p),timeout:options?.timeoutMs??35000});let value;
  if(method==='serviceNodes.get')value={...serviceOwner,serviceName:'agent-manager'};
  else if(method==='tools.list')value={provider:{node:{serviceNodeId:target.serviceNodeId}},items:[{qualifiedName:p.namespace+'.'+p.namePrefix,definition:{namespace:p.namespace,name:p.namePrefix,interfaceVersion:'1.0.0',inputSchema:{},outputSchema:{}},definitionHash:hash('e'),catalogHash:target.catalogHash}],nextCursor:null};
  else if(method==='tools.call'){assert.equal(p.qualifiedName,'agent.status');value={...status,...input.status};}
  else assert.fail('Unexpected RPC '+method);
  steps.push({method,value:clone(value)});return clone(value);
 }};
 const store=new SecretaryStore(client,rootId),guard=async()=>{guardCalls++;if(input.guardFailure)throw new IvyError('fixture_guard_failed','Fixture guard failure.');};
 for(const row of input.documents){const revisions=row.revisions??[{revision:row.revision??1,value:row.value}],current=revisions.find(v=>v.revision===(row.revision??1));store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(row.id,row.key,row.name,row.revision??1,canonical(current.value));for(const r of revisions)store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(row.id,r.revision,canonical(r.value));}
 for(const b of input.blobs)store.db.prepare('INSERT INTO technical_blobs VALUES(?,?,?,?)').run(b.id,b.key,b.name,Buffer.from(b.bytes,'base64'));
 const microsoft=new MicrosoftStore(store,guard),native=new NativeOwner(client,settings.identity.principalId,target),boundaries=new MicrosoftEpochBoundaries(store,native,input.namespace??'secretary/outlook');
 const results=[];
 for(const raw of actions)try{const action=resolve(raw,results);let value;
  if(action.method==='read')value=await microsoft.read(action.key,action.selected,action.immutable,action.name);
  else if(action.method==='find')value=await microsoft.find(action.key,action.name,action.immutable);
  else if(action.method==='create')value=await microsoft.create(action.key,action.name,action.value,action.immutable);
  else if(action.method==='amend')value=await microsoft.amend(action.key,action.current,action.value);
  else if(action.method==='save')value=await microsoft.saveOperation(action.plan??pin,action.slot??0,action.operation);
  else if(action.method==='operation')value=await microsoft.operation(action.plan??pin,action.slot??0,action.value);
  else if(action.method==='history'){await microsoft.checkCallHistory(action.key??'secretary/outlook-call',{pin:{objectId:'call',revision:1},value:action.value},native);value=null;}
  else if(action.method==='boundaryRead')value=await boundaries.read(pin,action.call??originalCall,action.previousEpoch??'old-epoch');
  else if(action.method==='boundaryRecord'){await boundaries.record(pin,action.call??originalCall,action.previousEpoch??'old-epoch',action.absence??absence,guard);value=null;}
  else assert.fail(action.method);results.push({value:value??null});
 }catch(e){results.push(result(e));}
 const docs=store.db.prepare('SELECT id,key,name,current_revision,value_json FROM technical_documents ORDER BY key,name').all(),blobs=store.db.prepare('SELECT * FROM technical_blobs ORDER BY key,name').all();
 const ids=new Map([...docs.map(r=>[r.id,'document:'+r.key+':'+r.name]),...blobs.map(r=>[r.id,'blob:'+r.key+':'+r.name])]);
 const normalize=v=>typeof v==='string'?(ids.get(v)??v):Array.isArray(v)?v.map(normalize):v&&typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,c])=>[k,normalize(c)])):v;
 const retained={documents:docs.map(r=>({id:r.id,key:r.key,name:r.name,revision:r.current_revision,value:JSON.parse(r.value_json)})),revisions:store.db.prepare('SELECT id,revision,value_json FROM technical_revisions ORDER BY id,revision').all().map(r=>({id:r.id,revision:r.revision,value:JSON.parse(r.value_json)})),blobs:blobs.map(r=>({id:r.id,key:r.key,name:r.name,bytes:Buffer.from(r.bytes).toString('base64')}))};
 retained.revisions.sort((a,b)=>normalize(a.id).localeCompare(normalize(b.id),'en')||a.revision-b.revision);
 input.steps=steps;store.close();rows.push({input,expected:normalize({results,trace,guardCalls,retained})});
}
for(const [key,name]of Object.entries(microsoftDefinitions)){
 const value=values[name],changed={...value,schemaVersion:1,...(name.endsWith('Publication')?{captured:[pin]}:name.endsWith('Call')?{seen:null,observation:null,slot:1}:name.endsWith('Head')?{nextPollAt:at}:name.endsWith('Evidence')?{gap:'retained gap'}:{previousCursor:'retained cursor'})};
 await add(name+' create replay amend',[{method:'create',key,name:'workflow',value,immutable:true},{method:'create',key,name:'workflow',value,immutable:true},{method:'read',key,selected:'$0.value.pin',immutable:true},{method:'amend',key,current:'$0.value',value:changed},{method:'find',key,name:'workflow'},{method:'read',key,selected:'$3.value.pin',immutable:true},{method:'amend',key,current:'$0.value',value:changed},{method:'amend',key,current:'$3.value',value:changed}]);
}
const doc={id:'fixed-document',key:'secretary/outlook-publication',name:'workflow',value:values.OutlookPublication};
for(const action of [{method:'read',key:doc.key,selected:doc.id,name:'other'},{method:'read',key:doc.key,selected:{objectId:doc.id,revision:2}},{method:'read',key:doc.key,selected:'missing'},{method:'find',key:doc.key,name:'absent'},{method:'create',key:doc.key,name:doc.name,value:{...doc.value,captured:[pin]},immutable:true},{method:'create',key:doc.key,name:doc.name,value:{...doc.value,captured:[pin]}},{method:'read',key:doc.key,selected:doc.id,name:''}])await add('document '+JSON.stringify(action),[action],{documents:[doc]});
await add('invalid stored schema',[{method:'read',key:doc.key,selected:doc.id}],{documents:[{...doc,value:{}}]});
await add('unpaired local name admission',[{method:'find',key:doc.key,name:'\ud800'}],{documents:[{...doc,name:'\ufffd'}]});
await add('guard stops create',[{method:'create',key:doc.key,name:'new',value:doc.value}],{guardFailure:true});
await add('validation before guard',[{method:'create',key:doc.key,name:'new',value:{}}],{guardFailure:true});
await add('unchanged amendment skips guard',[{method:'amend',key:doc.key,current:{pin:{objectId:doc.id,revision:1},value:doc.value},value:doc.value}],{documents:[doc],guardFailure:true});
await add('changed amendment checks guard',[{method:'amend',key:doc.key,current:{pin:{objectId:doc.id,revision:1},value:doc.value},value:{...doc.value,captured:[pin]}}],{documents:[doc],guardFailure:true});
await add('original historical pin and current immutable head',[{method:'read',key:doc.key,selected:{objectId:doc.id,revision:1},immutable:true},{method:'find',key:doc.key,name:doc.name,immutable:true},{method:'read',key:doc.key,selected:doc.id}],{documents:[{...doc,revision:2,revisions:[{revision:1,value:doc.value},{revision:2,value:{...doc.value,captured:[pin]}}]}]});
const largeOperation={...operation('succeeded'),reply:{result:{body:'x'.repeat(1048592)}}};
await add('native reply larger than schema frame remains complete',[{method:'save',operation:largeOperation},{method:'operation',value:'$0.value'}]);
const operationName='microsoft-operation-'+hashJson([pin,0,digest(Buffer.from(canonical(operation('succeeded'))))]).slice(7);
await add('same immutable blob name cannot change bytes',[{method:'save',operation:operation('succeeded')}],{blobs:[{id:'fixed-blob',key:'secretary/microsoft-native',name:operationName,bytes:Buffer.from('{}').toString('base64')}]});
for(const phase of ['accepted','dispatched','outcome_unknown','succeeded','failed']){
 await add('complete '+phase+' blob and exact replay',[{method:'save',operation:operation(phase)},{method:'save',operation:operation(phase)},{method:'operation',value:'$0.value'}]);
 await add('history '+phase,[{method:'save',operation:operation(phase)},{method:'history',value:{...originalCall,seen:phase==='succeeded'||phase==='failed'?null:'$0.value',observation:phase==='succeeded'||phase==='failed'?'$0.value':null}}]);
}
await add('no observed call needs no owner',[{method:'history',value:originalCall}]);
await add('guard stops receipt storage',[{method:'save',operation:operation('succeeded')}],{guardFailure:true});
await add('invalid native receipt before guard',[{method:'save',operation:{}}],{guardFailure:true});
for(const [label,current,previous]of [
 ['pending accepted to terminal',operation('succeeded'),operation('accepted')],['pending dispatched to failed',operation('failed'),operation('dispatched')],['unknown to success',operation('succeeded'),operation('outcome_unknown')],
 ['terminal cannot be pending',operation('succeeded'),operation('succeeded')],['timestamp regression',{...operation('succeeded'),updatedAt:'2026-01-01T00:00:00.001Z'},{...operation('dispatched'),updatedAt:'2026-01-01T00:00:00.002Z'}],
 ['original epoch change',{...operation('succeeded'),epoch:'other'},operation('dispatched')],['request identity change',{...operation('succeeded'),requestId:43},operation('dispatched')],['wrong caller',{...operation('succeeded'),callerPrincipalId:'other'},operation('accepted')],
 ['created before prepared',{...operation('succeeded'),createdAt:'2025-12-31T23:59:59.999Z'},operation('accepted')]])await add(label,[{method:'save',operation:previous},{method:'save',operation:current},{method:'history',value:{...originalCall,seen:'$0.value',observation:'$1.value'}}]);
await add('terminal seen without observation',[{method:'save',operation:operation('succeeded')},{method:'history',value:{...originalCall,seen:'$0.value'}}]);
await add('pending in observation',[{method:'save',operation:operation('dispatched')},{method:'history',value:{...originalCall,observation:'$0.value'}}]);
for(const variant of ['missing','revision','hash','length','multiple','noncanonical','utf8','bom','invalidJson','invalidOperation','otherPlanSlot']){
 let bytes=Buffer.from(canonical(operation('succeeded'))),chunks=[{objectId:'fixed-blob',revision:1}],contentHash,byteLength;
 if(variant==='noncanonical')bytes=Buffer.from(JSON.stringify(operation('succeeded')));
 if(variant==='utf8')bytes=Buffer.from([255]);if(variant==='bom')bytes=Buffer.concat([Buffer.from([239,187,191]),bytes]);
 if(variant==='invalidJson')bytes=Buffer.from('{');if(variant==='invalidOperation')bytes=Buffer.from('{}');
 contentHash=digest(bytes);byteLength=bytes.length;if(variant==='hash')contentHash=hash('f');if(variant==='length')byteLength++;
 if(variant==='revision')chunks[0].revision=2;if(variant==='multiple')chunks.push(clone(chunks[0]));
 await add('blob '+variant,[{method:'operation',value:{byteLength,contentHash,chunks},...(variant==='otherPlanSlot'?{plan:{objectId:'different',revision:8},slot:6}:{})}],{blobs:variant==='missing'?[]:[{id:'fixed-blob',key:'secretary/microsoft-native',name:'original-name',bytes:bytes.toString('base64')}]});
}
await add('original boundary exact replay',[{method:'boundaryRead'},{method:'boundaryRecord'},{method:'boundaryRead'},{method:'boundaryRecord'}]);
await add('boundary guard refusal',[{method:'boundaryRecord'}],{guardFailure:true});
await add('same epoch cannot establish boundary',[{method:'boundaryRecord',previousEpoch:status.epoch}]);
await add('different absence epoch',[{method:'boundaryRecord',absence:{...absence,epoch:'other'}}]);
await add('different absence ID',[{method:'boundaryRecord',absence:{...absence,operationId:'other'}}]);
await add('changed owner status fails',[{method:'boundaryRecord'}],{status:{nativeExecutableHash:hash('f')}});
await add('unready owner fails',[{method:'boundaryRecord'}],{status:{state:'starting'}});
const boundaryName='epoch-'+hashJson([pin,originalCall.operationId,originalCall.method,originalCall.params,originalCall.definitionHash]).slice(7);
const boundary={schemaVersion:1,plan:pin,callHash:hashJson(Object.fromEntries(['operationId','method','params','definitionHash'].map(k=>[k,originalCall[k]]))),previousEpoch:'old-epoch',absence,status};
for(const [field,value]of [['schemaVersion',2],['plan',{objectId:'other',revision:1}],['callHash',hash('f')],['previousEpoch','other'],['absence',{...absence,operationId:'other'}],['absence',{...absence,epoch:'other'}],['status',{...status,epoch:'old-epoch'}]])await add('stored boundary '+field,[{method:'boundaryRead'}],{documents:[{id:'boundary',key:'secretary/outlook/epoch-boundary',name:boundaryName,value:{...boundary,[field]:value}}]});
await add('new status cannot replace retained boundary',[{method:'boundaryRecord'}],{documents:[{id:'boundary',key:'secretary/outlook/epoch-boundary',name:boundaryName,value:{...boundary,status:{...status,observedAt:'2026-01-01T00:00:00.001Z'}}}]});
await add('alternate media namespace',[{method:'boundaryRecord'},{method:'boundaryRead'}],{namespace:'secretary/media'});
for(const label of ['original boundary exact replay','alternate media namespace','pending accepted to terminal','blob bom','blob otherPlanSlot'])assert(rows.find(r=>r.input.label===label).expected.results.every(v=>'value'in v),label);
console.log(JSON.stringify({target,caller:settings.identity.principalId,validation,bundles,registry:microsoftRegistry(),definitions:microsoftDefinitions,methods:microsoftMethods,rows}));
