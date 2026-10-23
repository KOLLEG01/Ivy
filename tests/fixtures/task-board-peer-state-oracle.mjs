import {readFileSync,existsSync} from 'node:fs';
import {stripTypeScriptTypes,registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
registerHooks({resolve(specifier,context,next){
 if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
 return next(specifier,context);
}});
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');

const source=readFileSync('services/task-board/src/runtime/task-coordination.ts','utf8');
const subject=new Function('hashJson',stripTypeScriptTypes(source.slice(source.indexOf('export async function workspaceIdentity'),source.indexOf('export async function checkBlocker')).replaceAll('export ',''))+'\nreturn peerState;')(hashJson);
const task=(path='/work',host='host',state='todo')=>({workflowState:state,claim:path===null?null:{workspace:{canonicalCwd:path,hostId:host}},lastRun:null});
const pin=(objectId,revision=1)=>({objectId,revision}),record=(id,value,key='task-board/task',revision=1)=>({key,pin:pin(id,revision),value,metadata:{parentId:'root'}});
const page=(ids,nextCursor=null)=>({items:ids.map(id=>typeof id==='string'?pin(id):id),nextCursor});
const cases=[];const add=(name,input=task(),records=[],pages=[],options={})=>cases.push({name,task:input,taskId:'self',records,pages,...options});
add('no workspace returns empty without paging',task(null));
add('no workspace with null prior Run',{...task(null),lastRun:pin('run')},[record('run',{workspace:null},'task-board/run')]);
add('present claim skips missing prior Run',{...task(),lastRun:pin('missing')},[],[page([])]);
add('self summary is not read',task(),[],[page(['self'])]);
add('same workspace peer',task(),[record('peer',task('/work','host','waiting'))],[page(['peer'])]);
add('different host stays separate',task(),[record('peer',task('/work','Host'))],[page(['peer'])]);
add('POSIX workspace stays case sensitive',task(),[record('peer',task('/Work'))],[page(['peer'])]);
add('missing peer workspace stays separate',task(),[record('peer',task(null))],[page(['peer'])]);
for(const [name,own,peer] of [
 ['drive spelling','C:\\Work\\Path\\','c:/work/path'],
 ['UNC spelling','\\\\Server\\Share\\Σ\\','//server/share/σ'],
 ['Unicode drive lowercase','C:/İ/Σ/\ud800','c:/i\u0307/σ/\ud800'],
 ['only one trailing slash removed','/work//','/work/'],
 ['no dot segment normalization','C:/work/../path','c:/path'],
 ['relative drive case is retained','C:Work','c:work'],
 ['POSIX backslashes become separators','/a\\b','/a/b'],
 ['UNC and ordinary slash roots differ','//Server','/server'],
 ['root slash identity','/',''],
 ['exact unmatched surrogate path','/work/\ud800','/work/\ud800'],
 ['different unmatched surrogate path','/work/\ud800','/work/\udfff'],
])add(name,task(own),[record('peer',task(peer))],[page(['peer'])]);
add('own prior Run supplies workspace',{...task(null),lastRun:pin('own-run',2)},[record('own-run',{workspace:{hostId:'host',canonicalCwd:'/work'}},'task-board/run',2),record('peer',task())],[page(['peer'])]);
add('peer prior Run supplies workspace',task(),[record('peer',{...task(null),lastRun:pin('peer-run',3)}),record('peer-run',{workspace:{hostId:'host',canonicalCwd:'/work'}},'task-board/run',3)],[page(['peer'])]);
add('claim workspace wins over a different prior Run',task(),[record('peer',{...task(),lastRun:pin('missing-run')})],[page(['peer'])]);
const sortable=['z','a','A','a:0','é','😀','\ud800','\udfff'];
add('UTF16 sort retains peer identifiers',task(),sortable.map((id,n)=>record(id,task('/work','host',['todo','in_progress','waiting','review'][n%4]))),[page(sortable)]);
add('page order does not affect sorted hash',task(),sortable.map(id=>record(id,task())),[page(sortable.slice(4),'next'),page(sortable.slice(0,4))]);
add('duplicate summary remains in hash',task(),[record('peer',task())],[page(['peer'],'next'),page(['peer'])]);
add('empty page with continuation',task(),[record('peer',task())],[page([],'next'),page(['peer'])]);
add('empty string cursor stops',task(),[],[page([],''),page(['missing'])]);
add('repeated cursor is passed through',task(),[record('peer',task())],[page([],'repeat'),page(['peer'],'repeat'),page([])]);
add('pinned revision wins over later latest',task(),[record('peer',task('/work','host','waiting'),'task-board/task',1),record('peer',task('/elsewhere','host','done'),'task-board/task',2)],[page(['peer'])]);
add('summary fields cannot replace pinned state',task(),[record('peer',task('/work','host','review'),'task-board/task',3)],[page([{...pin('peer',3),fields:{'data:/workflowState':'todo'}}])]);
add('one hundred and one peers span pages',task(),Array.from({length:101},(_,n)=>record('peer-'+n,task())),[page(Array.from({length:100},(_,n)=>'peer-'+n),'last'),page(['peer-100'])]);
for(const [name,where,code,outcome] of [['page failure','page','service_unavailable','unknown'],['missing peer','read','not_found','not_executed'],['archived peer','read','task_board_record_archived','not_executed'],['wrong root peer','read','task_board_scope_mismatch','unknown']])add(name,task(),[record('peer',task())],[page(['peer'])],{fault:{where,code,outcome}});
add('own Run failure prevents paging',{...task(null),lastRun:pin('own-run')},[],[],{fault:{where:'read',code:'service_unavailable',outcome:'unknown'}});
add('peer Run failure remains visible',task(),[record('peer',{...task(null),lastRun:pin('peer-run')})],[page(['peer'])],{fault:{where:'read',at:2,code:'service_unavailable',outcome:'unknown'}});

const out=[];
for(const spec of cases){
 const boundaries=[];let pageIndex=0,readIndex=0;
 const error=e=>({code:e.code,message:e.message,outcome:e.outcome,details:e.details??null});
 const effect=(kind,args,fn)=>{const row={kind,args:structuredClone(args)};boundaries.push(row);try{const index=kind==='read'?++readIndex:pageIndex+1;if(spec.fault?.where===kind&&index===(spec.fault.at??1))throw new IvyError(spec.fault.code,'Injected '+kind,spec.fault.outcome,{boundary:kind});const value=fn();row.value=structuredClone(value);return value;}catch(e){row.error=error(e);throw e;}};
 const store={async page(key,query){return effect('page',[key,query],()=>{assert.equal(key,'task-board/task');assert.deepEqual(query.where,{op:'in',field:'data:/workflowState',value:['todo','in_progress','waiting','review']});assert.equal(query.limit,100);assert(pageIndex<spec.pages.length,'Unexpected page');return spec.pages[pageIndex++];});},async read(key,address,primary=true,archived=false){return effect('read',[key,address,primary,archived],()=>{assert.equal(primary,true);assert.equal(archived,false);const records=spec.records.filter(r=>r.key===key&&r.pin.objectId===(typeof address==='string'?address:address.objectId));const found=typeof address==='string'?records.at(-1):records.find(r=>r.pin.revision===address.revision);assert(found,'Unexpected pin '+JSON.stringify(address));return found;});}};
 let expected;try{expected={ok:await subject(store,spec.task,spec.taskId)};}catch(e){if(!e.code)throw e;expected={error:error(e)};}
 out.push({...spec,boundaries,expected,expectedHash:hashJson(expected)});
}
assert.equal(out[0].expected.ok,hashJson([]));assert.equal(out.find(v=>v.name==='self summary is not read').boundaries.length,1);
process.stdout.write(JSON.stringify(out));
