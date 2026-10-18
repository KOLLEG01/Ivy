import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createRequire } from 'node:module';
import { schemaSamples } from '../fixtures/schema-samples.mjs';
const root=resolve(process.env.IVY_SECRETARY_ORACLE??'.local/build/secretary');
const load=p=>import(pathToFileURL(resolve(root,p)));
const { secretaryBrowserNotice }=await load('services/secretary/src/browser-notice.js');
const { SecretaryStore }=await load('services/secretary/src/store.js');
const { IvyError, hashJson, canonical, parseOperationId }=await load('packages/sdk/src/node.js');
const { Ajv2020 }=createRequire(import.meta.url)('ajv/dist/2020.js');
const schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const sample=schemaSamples(ajv,[schema]).find(r=>r.reference.endsWith('#/$defs/SecretaryConfiguration')&&r.valid);
assert.ok(sample);const seed=JSON.parse(sample.input);
const rootId='00000000-0000-4000-8000-000000000001',objectId='00000000-0000-4000-8000-000000000002';
const at=1800000000000,key='secretary/browser-notice',rows=[];
async function add(options={}){
 const input={node:'fixture.secretary',tag:'fixture-tag',text:'Notice',repeats:2,existing:false,capacity:10000,configuration:'enabled',failAt:[],...options};
 const steps=[],trace=[],ids=[];let timeCalls=0;
 const value={...seed,browserNotifications:input.configuration!=='disabled'};
 if(input.configuration==='default')delete value.browserNotifications;
 const client={async request(method,params,options){
  const copied=structuredClone(params);
  if(method==='events.publish'){parseOperationId(copied.mutationId);if(!ids.includes(copied.mutationId))ids.push(copied.mutationId);copied.mutationId=`<scoped-${ids.indexOf(copied.mutationId)}>`;}
  trace.push({method,params:copied,timeout:options?.timeoutMs??35000});
  if(input.failAt.includes(trace.length)){const error={code:'fixture_failure',message:'Fixture failure.',outcome:input.outcome??'not_executed'};steps.push({method,error});throw new IvyError(error.code,error.message,error.outcome);}
  let result;
  if(method==='objects.query')result={items:input.configuration==='absent'?[]:[{objectId,revision:1}],nextCursor:null};
  else if(method==='objects.read')result={object:{id:objectId,parentId:rootId,effectivelyArchived:false,contractKey:'secretary/configuration'},revision:{revision:1,contractVersion:'1.1.0',mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:{},createdAt:'2026-01-01T00:00:00.000Z'},content:{encoding:'json',value}};
  else if(method==='system.status')result={runtimeEpoch:'fixture-epoch'};
  else if(method==='events.publish')result={};else throw Error(`Unexpected ${method}`);
  steps.push({method,value:structuredClone(result)});return result;
 }};
 const store=new SecretaryStore(client,rootId,undefined,{count:input.capacity,bytes:256*1024*1024});
 if(input.existing)store.technicalCreate(key,input.tag,{expiresAt:1});
 const engine={client,store,owner:{serviceNodeId:input.node},now:()=>new Date(at+(timeCalls++)*1000)};
 const results=[];
 for(let i=0;i<input.repeats;i++)try{await secretaryBrowserNotice(engine,input.tag,input.text);results.push({value:null});}catch(cause){const e=IvyError.from(cause);results.push({code:e.code,message:e.message,outcome:e.outcome});}
 const saved=store.technicalNamed(key,input.tag)?.value??null;store.close();
 rows.push({input:{...input,steps},expected:{results,trace,saved,timeCalls,remaining:0}});
}
for(const configuration of ['enabled','disabled','default','absent'])for(const existing of [false,true])await add({configuration,existing});
for(const node of ['space node','a&b=c?/#','!*\'()~_- .','é/😀','\ud800','\udc00','nul\0node'])await add({node});
for(const text of ['', 'a'.repeat(301),'a'.repeat(299)+'😀','😀'.repeat(151),'\ud800\udc00\ud800','line\n\ttext'])await add({text});
for(const tag of ['', 'é😀','\ud800','a\0b'])await add({tag});
for(const outcome of ['not_executed','unknown','completed'])for(const failAt of [[1],[2],[3],[4]])await add({failAt,outcome});
await add({capacity:0});
process.stdout.write(JSON.stringify({root:rootId,at,rows}));
