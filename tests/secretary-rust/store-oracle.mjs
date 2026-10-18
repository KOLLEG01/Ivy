import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createRequire } from 'node:module';
import { schemaSamples } from '../fixtures/schema-samples.mjs';
const root=resolve(process.env.IVY_SECRETARY_ORACLE??'.local/build/secretary');
const load=p=>import(pathToFileURL(resolve(root,p)));
const { SecretaryStore, mutation, conflict }=await load('services/secretary/src/store.js');
const { IvyError, hashJson, canonical, parseOperationId }=await load('packages/sdk/src/node.js');
const { contractVersion, readableVersions, validate }=await load('services/secretary/src/schema.js');
const { Ajv2020 }=createRequire(import.meta.url)('ajv/dist/2020.js');
const schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const samples=schemaSamples(ajv,[schema]);
const definitions={'secretary/item':'Item','secretary/source':'Source','secretary/operation':'Accepted','secretary/configuration':'SecretaryConfiguration','secretary/assignment':'Assignment','secretary/schedule-progress':'ScheduleProgress','secretary/event-progress':'EventProgress','secretary/execution':'Execution'};
const seeds=Object.fromEntries(Object.entries(definitions).map(([key,name])=>{const row=samples.find(r=>r.reference.endsWith('#/$defs/'+name)&&r.valid);assert.ok(row,name);const value=JSON.parse(row.input);validate(name,value);return [key,value];}));
const rootId='00000000-0000-4000-8000-000000000001',objectId='00000000-0000-4000-8000-000000000002';
const id='fixture-epoch:1800000000000:store-write',at='2026-01-01T00:00:00.000Z';
const pin={objectId,revision:2},dest={createName:'fixture'};
const probe=new SecretaryStore({request(){throw Error('unexpected');}},rootId);
function read(key,value=seeds[key],revision=2){return {object:{id:objectId,parentId:rootId,effectivelyArchived:false,contractKey:key},revision:{revision,contractVersion:contractVersion(key),mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:probe.objectWriteRequest(key,value,dest,id).references,createdAt:at},content:{encoding:'json',value}};}
const step=(method,value)=>({method,value});
const fail=(method,code='fixture_failure',outcome='not_executed')=>({method,error:{code,message:'Fixture failure.',outcome}});
const query=(items=[],nextCursor=null)=>step('objects.query',{items,nextCursor});
const item={objectId,revision:2};
const rows=[];
async function add(method,args,steps=[],locals=[]){
  const input={method,args,steps,locals};const pending=structuredClone(steps),trace=[];
  const client={async request(method,params,options){
    const copied=structuredClone(params);
    if(input.method==='amend'&&method==='objects.write'){parseOperationId(copied.mutationId);copied.mutationId='<scoped-operation>';}
    trace.push({method,params:copied,timeout:options?.timeoutMs??35000});
    const next=pending.shift();assert.ok(next,`Unexpected ${method}`);assert.equal(next.method,method);
    if(next.error)throw new IvyError(next.error.code,next.error.message,next.error.outcome);
    return structuredClone(next.value);
  }};
  const store=new SecretaryStore(client,rootId);
  for(const local of locals){const encoded=canonical(local.value);store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(local.id,local.key,local.name,1,0,null,encoded);store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(local.id,1,encoded);}
  let result;
  try{const copied=structuredClone(args);let value;if(method==='mutation')value=mutation(...copied);else if(method==='conflict'){const e=copied[0];value=conflict(new IvyError(e.code,'Fixture failure.',e.outcome));}else value=await store[method](...copied);result={value:value??null};}
  catch(cause){const e=IvyError.from(cause);result={code:e.code,message:e.message,outcome:e.outcome};}
  finally{store.close();}
  rows.push({input,expected:{result,trace,remaining:pending.length}});
}
for(const [key,value] of Object.entries(seeds)){
  if(['secretary/source','secretary/operation'].includes(key)){
    await add('objectWriteRequest',[key,value,dest,id]);
    const locals=[{key,value,id:objectId,name:'fixture'}];
    await add('read',[key,objectId],[],locals);await add('read',[key,{objectId,revision:1}],[],locals);
    await add('named',[key,'fixture'],[],locals);await add('named',[key,'absent'],[],locals);
    await add('read',[key,objectId]);await add('read',[key,{objectId,revision:2}],[],locals);
    continue;
  }
  const original=read(key);
  for(const version of [...readableVersions(key),'9.0.0','1.0','']){const changed=structuredClone(original);changed.revision.contractVersion=version;await add('decode',[key,changed]);}
  for(const [path,bad] of [['object.parentId','other'],['object.contractKey','secretary/source'],['object.effectivelyArchived',true],['revision.mediaType','text/plain'],['content.encoding','base64'],['revision.contentHash','sha256:'+'0'.repeat(64)],['revision.byteLength',0],['revision.references',{extra:{objectId,revision:1}}]]){const changed=structuredClone(original);const [part,field]=path.split('.');changed[part][field]=bad;await add('decode',[key,changed]);}
  for(const invalid of [null,{}, {...value,unexpected:true}]){const changed=structuredClone(original);changed.content.value=invalid;changed.revision.contentHash=hashJson(invalid);changed.revision.byteLength=Buffer.byteLength(canonical(invalid));await add('decode',[key,changed]);await add('objectWriteRequest',[key,invalid,dest,id]);}
  await add('objectWriteRequest',[key,value,dest,id]);await add('objectWriteRequest',[key,value,{object:pin},id]);await add('objectWriteRequest',[key,value,dest,'invalid']);
  await add('read',[key,objectId],[step('objects.read',original)]);await add('read',[key,pin],[step('objects.read',original)]);
}
const key='secretary/assignment',value=seeds[key],original=read(key),doc=probe.decode(key,original),nextValue={...value,description:'Changed'},nextRead=read(key,nextValue,3),nextDoc=probe.decode(key,nextRead);
await add('named',[key,'fixture'],[query()]);await add('named',[key,'fixture'],[query([item]),step('objects.read',original)]);
await add('named',[key,'\ud800'],[query()]);await add('named',[key,'fixture'],[query([item,item])]);await add('named',[key,'fixture'],[query([], 'more')]);
await add('create',[key,'fixture',null,id],[query([item]),step('objects.read',original)]);
await add('create',[key,'fixture',value,id],[query(),step('objects.write',{object:{id:objectId},revision:{revision:2}}),step('objects.read',original)]);
for(const outcome of ['not_executed','unknown','completed']){
 await add('create',[key,'fixture',value,id],[query(),fail('objects.write','fixture_failure',outcome),query([item]),step('objects.read',original)]);
 await add('create',[key,'fixture',value,id],[query(),fail('objects.write','fixture_failure',outcome),query()]);
 for(const code of ['revision_conflict','mutation_conflict','fixture_failure']){
  const start=[step('system.status',{runtimeEpoch:'fixture-epoch'}),fail('objects.write',code,outcome)];
  await add('amend',[doc,key,nextValue],code==='fixture_failure'?start:[...start,step('objects.read',nextRead)]);
  await add('conflict',[{code,outcome}]);
 }
}
await add('create',[key,'fixture',value,id],[query(),step('objects.write',{object:{id:objectId},revision:{revision:2}}),fail('objects.read'),query([item]),step('objects.read',original)]);
await add('create',[key,'fixture',value,id],[query(),fail('objects.write'),fail('objects.query','lookup_failure')]);
await add('amend',[doc,key,value]);
await add('amend',[doc,key,nextValue],[step('system.status',{runtimeEpoch:'fixture-epoch'}),step('objects.write',{object:{id:objectId},revision:{revision:3}}),step('objects.read',nextRead)]);
await add('amend',[doc,key,nextValue],[step('system.status',{runtimeEpoch:'fixture-epoch'}),fail('objects.write','revision_conflict'),step('objects.read',original)]);
await add('amend',[doc,key,nextValue],[fail('system.status')]);
await add('original',[key,{...doc,pin:{objectId,revision:1}}]);await add('original',[key,doc],[step('objects.read',read(key,value,1))]);
const execution=structuredClone(seeds['secretary/execution']);execution.trigger.payload={kind:'secretary.object.batch',objectId,revision:8,beforeRevision:3};
await add('objectWriteRequest',['secretary/execution',execution,dest,id]);await add('decode',['secretary/execution',read('secretary/execution',execution)]);
const media={contractKey:'fixture/media',pin:null,contentHash:null,name:null,mediaType:null,errorCode:null};
const withMedia={...seeds['secretary/item'],media:[media,{...media,pin},{...media,pin:{objectId,revision:1}}]};
await add('objectWriteRequest',['secretary/item',withMedia,dest,id]);await add('decode',['secretary/item',read('secretary/item',withMedia)]);
const wrongRefs=read('secretary/item',withMedia);wrongRefs.revision.references={'media-0':pin};await add('decode',['secretary/item',wrongRefs]);
for(const parts of [[],['x'],[id],['invalid',[id]],['epoch:999999999999:early'],['\ud800',{nested:id}],[{b:id,a:'other:1800000000000:second'}],[{b:id,2:'numeric:1800000000000:first',1:'numeric:1800000000000:zero'}]])await add('mutation',parts);
probe.close();
process.stdout.write(JSON.stringify({root:rootId,rows}));
