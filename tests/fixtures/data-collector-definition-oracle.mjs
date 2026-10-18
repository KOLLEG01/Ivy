import {readFileSync,existsSync} from 'node:fs';
import {stripTypeScriptTypes,registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
import {schemas} from './data-collector-schema-oracle.mjs';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {canonical,digest}=await import('../../packages/contracts/src/canonical.ts');
const sdk=/import\s*\{[^}]+\}\s*from\s*"[^\"]*packages\/sdk\/src\/node\.js";/;
const load=async(name,replacements)=>{let source=stripTypeScriptTypes(readFileSync(new URL('../../services/data-collector/src/'+name+'.ts',import.meta.url),'utf8'),{mode:'transform'});for(const [pattern,replacement]of replacements)source=source.replace(pattern,replacement);return import('data:text/javascript;base64,'+Buffer.from(source).toString('base64'));};
let next=0;
globalThis.__collectorDefinitions={IvyError,requireThat,canonical,digest,...schemas,newOperationId:async()=>`fresh-${++next}`,serviceTools:()=>{throw Error('Unexpected service dispatch');}};
const {CollectorStore}=await load('store',[[sdk,'const {digest,requireThat}=globalThis.__collectorDefinitions;']]);
const {CollectorDefinitions}=await load('definitions',[[sdk,'const {canonical,IvyError,requireThat,newOperationId,serviceTools}=globalThis.__collectorDefinitions;'],[/import\s*\{[^}]+\}\s*from\s*".\/schema.js";/,'const {assignmentDocument,definitionDocument,observationDocument,readSchema,scriptDocument,taskSchema,updateSchema}=globalThis.__collectorDefinitions;']]);
const doc=(value,revision=2,extra={})=>({object:{id:'target',contractKey:'data-collector/definition',effectivelyArchived:false,...extra},revision:{revision,references:{}},content:{encoding:'json',value}});
const value={schemaVersion:1,fixture:'retained',lone:'\ud800'};
const previous=doc({schemaVersion:1,fixture:'before'},1);
const cas={mutationId:'old-operation',contractVersion:'1.0.0',references:{source:{objectId:'source',revision:1}},objectId:'target',expectedRevision:1,content:{encoding:'json',value}};
const creation={...cas,create:{name:'definition-fixed',parentId:'catalog',ownerObjectId:null,contractKey:'data-collector/definition'}};delete creation.objectId;delete creation.expectedRevision;
const missing={code:'revision_not_found',message:'Missing pinned revision.',outcome:'not_executed'};
const conflict={code:'revision_conflict',message:'Stale CAS.',outcome:'not_executed'};
const cases=[
  {name:'lost creation reply with later revision',retained:creation,replies:[{items:[{objectId:'target'}]},doc(value,1)]},
  {name:'lost CAS reply with later revision',retained:cas,replies:[doc(value)]},
  {name:'another writer won the CAS',retained:cas,replies:[doc({other:true}),{error:conflict}]},
  {name:'uncommitted CAS resumes',retained:cas,replies:[{error:missing},doc(value),doc(value)]},
  {name:'ambiguous creation',retained:creation,replies:[{items:[{objectId:'one'},{objectId:'two'}]}]},
  {name:'archived successor',retained:cas,replies:[doc(value,2,{effectivelyArchived:true})]},
  {name:'foreign successor',retained:cas,replies:[doc(value,2,{contractKey:'foreign'})]},
  {name:'non JSON control content',retained:cas,replies:[{...doc(value),content:{encoding:'utf8',value:'text'}}]},
  {name:'fresh pinned update',previous,replies:[doc(value),doc(value)]},
  {name:'observation has no durable write journal',kind:'observation',previous,replies:[doc(value),doc(value,2,{contractKey:'data-collector/observation'})]},
];
const results=[];
for(const [index,item]of cases.entries()){
 const store=new CollectorStore(process.argv[3]+'-'+index+'.sqlite');const operation='fixture-\ud800';const key='definition-write:'+operation+':step';
 if(item.retained)store.setMetadata(key,JSON.stringify(item.retained));
 const calls=[];const replies=[...item.replies];
 const client={request:async(method,params)=>{calls.push({method,params:structuredClone(params),retained:store.metadata(key)});const reply=replies.shift();if(!reply)throw Error('Unexpected request');if(reply.error)throw new IvyError(reply.error.code,reply.error.message,reply.error.outcome);return reply;}};
 const definitions=new CollectorDefinitions('fixture-node',{store,client});let result=null,error=null;
 try{result=await definitions.write(item.kind??'definition',value,operation,'step',item.previous,cas.references);}catch(cause){const e=IvyError.from(cause);error={code:e.code,message:e.message,outcome:e.outcome};}
 results.push({case:item,operation,value,calls,result,error,retained:store.metadata(key)});store.close();
}
delete globalThis.__collectorDefinitions;process.stdout.write(JSON.stringify(results));
