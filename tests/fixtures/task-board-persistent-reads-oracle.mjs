// Execute unchanged, pinned service methods with finite SQLite/RPC receivers.
// This qualifies their owned branches, not Shared tree traversal or migrations.
import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
import {stripTypeScriptTypes} from 'node:module';
import {DatabaseSync} from 'node:sqlite';
import {parseArgs} from 'node:util';
import assert from 'node:assert/strict';
const {values}=parseArgs({options:{'source-root':{type:'string'},output:{type:'string'},proof:{type:'string'},reuse:{type:'string'}}});
for(const name of ['source-root','output','proof'])assert.ok(values[name],name);
const root=path.resolve(values['source-root']),hash=b=>'sha256:'+createHash('sha256').update(b).digest('hex');
const {IvyError,requireThat}=await import(pathToFileURL(path.join(root,'packages/contracts/src/errors.ts')));
const load=p=>fs.readFileSync(path.join(root,p),'utf8');
const store=load('services/task-board/src/runtime/store.ts'),comments=load('services/task-board/src/runtime/comment-store.ts');
const method=(text,start,end)=>{const from=text.indexOf(start),to=text.indexOf(end,from+start.length);assert.ok(from>=0&&to>from);return text.slice(from,to);};
const methods={failed:method(store,'  failedHandoff(','  async protectRunSources('),prefix:method(comments,'  async read(value:','  async write(task:'),sizes:method(store,'  async upgradeStored<','  withReadCache<')};
const constant=comments.match(/^export const commentContractKey = '([^']+)';/m);assert.ok(constant);
const reused=values.reuse?JSON.parse(fs.readFileSync(values.reuse)):null;
const cases=[];let upgrades=[],executed=0,reusedCount=0;
const classes=Object.fromEntries(Object.entries(methods).map(([name,text])=>[name,new Function('requireThat','upgradeRecord','commentContractKey',stripTypeScriptTypes('class Original {\n'+text+'\n}',{mode:'transform'})+'\nreturn Original;')(requireThat,(key,value)=>{upgrades.push({key,value:structuredClone(value)});return value;},constant[1])]));
const clone=structuredClone,wire=e=>({error:IvyError.from(e).toWire()}),q=(code='storage_full',message='Retained failure.')=>({phase:'failed',handoffRun:{objectId:'run'},error:{code,message},operationId:'original-op'});
const row=(id,value,patch={})=>({id,key:'task-board/operation',updatedAt:'2026-01-01T00:00:00.000Z',revision:1,value,...patch});
async function capture(kind,name,input){
 const prior=reused?.cases.find(c=>c.kind===kind&&c.name===name);
 if(prior&&prior.rawFailure?.name!=='ReferenceError'){assert.deepEqual(input,prior.input);cases.push(prior);reusedCount++;return;}
 executed++;
 const before=JSON.stringify(input),trace=[],rpc=clone(input.rpc??[]);upgrades=[];let output,rawFailure=null;
 const client={async request(method,params){trace.push({method,params:clone(params)});const next=rpc.shift();assert.ok(next,'Unexpected RPC');if(next.error)throw new IvyError(next.error.code,next.error.message,next.error.outcome,next.error.details);return clone(next.value);}};
 try{
  if(kind==='sql'){
   const db=new DatabaseSync(':memory:');
   if(!input.missingTables){db.exec('CREATE TABLE documents(id TEXT PRIMARY KEY,key TEXT,updated_at TEXT,current_revision INTEGER); CREATE TABLE revisions(id TEXT,revision INTEGER,value_json TEXT);');for(const r of input.rows??[]){db.prepare('INSERT INTO documents VALUES(?,?,?,?)').run(r.id,r.key,r.updatedAt,r.revision);db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(r.id,r.revision,r.raw??JSON.stringify(r.value));for(const older of r.older??[])db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(r.id,older.revision,JSON.stringify(older.value));}}
   try{output={value:classes.failed.prototype.failedHandoff.call({local:db},input.runId??'run')};}finally{db.close();}
  }else if(kind==='prefix') output={value:await classes.prefix.prototype.read.call({client,reader:{range(){throw Error('Shared tree reader is outside this oracle selection.');}}},clone(input.task),input.taskId??'task')};
  else output={value:await classes.sizes.prototype.upgradeStored.call({client},input.key??'task-board/task',clone(input.task))};
 }catch(e){rawFailure={name:e.name,code:e.code??null,message:e.message};output=wire(e);}
 assert.notEqual(rawFailure?.name,'ReferenceError','The original module context is incomplete');
 assert.equal(JSON.stringify(input),before);cases.push({kind,name,input,output,trace,upgradeBoundary:upgrades,rawFailure,rpcRemaining:rpc.length});
}
await capture('sql','no retained failure',{rows:[]});
for(const code of ['content_too_large','storage_full','quota_exceeded','database_error'])await capture('sql','accepted failure '+code,{rows:[row('one',q(code))]});
for(const code of ['internal_error','outcome_unknown','custom'])await capture('sql','other code does not override '+code,{rows:[row('one',q(code))]});
await capture('sql','only failed phase',{rows:[row('one',{...q(),phase:'publishing'})]});
await capture('sql','run identity remains exact',{rows:[row('one',{...q(),handoffRun:{objectId:'other'}})]});
await capture('sql','only Operation records',{rows:[row('one',q(),{key:'task-board/task'})]});
await capture('sql','newest matching update wins',{rows:[row('older',q('storage_full','First.')),row('newer',{...q('database_error','Latest.'),operationId:'latest-op'},{updatedAt:'2026-01-02T00:00:00.000Z'})]});
await capture('sql','current revision controls selection',{rows:[row('revised',q('quota_exceeded','Current.'),{revision:2,older:[{revision:1,value:q('storage_full','Obsolete.')} ]})]});
await capture('sql','UTF16 truncation and lossless identity',{rows:[row('long',{...q('storage_full','x'.repeat(1799)+'\ud800tail'),operationId:'op-\udfff'})]});
await capture('sql','array message follows original slice coercion',{rows:[row('array',q('storage_full',['a',null,1]))]});
await capture('sql','missing message is an original access failure',{rows:[row('missing',{...q(),error:{code:'storage_full'}})]});
await capture('sql','missing operationId remains absent',{rows:[row('missing',{phase:'failed',handoffRun:{objectId:'run'},error:{code:'storage_full',message:'No id.'}})]});
await capture('sql','malformed retained JSON fails',{rows:[row('bad',null,{raw:'{'})]});
await capture('sql','SQLite failure is a non-Ivy error',{missingTables:true});
const comment=(id,patch={})=>({commentId:id,sequence:1,body:'\ud800body\udfff',requests:[],responses:[],nativeInput:{state:'current'},handoff:{workflowState:'todo'},delivery:{state:'current'},extra:{keep:true},...patch});
const log=(comments,patch={})=>({object:{contractKey:'task-board/comments',ownerObjectId:null,parentId:'task',...patch},content:{encoding:'json',value:{taskId:'task',comments}}});
const task=(length=1,states=[{commentId:'c',nativeInput:{state:'old'},delivery:null}])=>({extra:'header',comments:[],commentDeliveries:[{commentId:'original-receipt'}],commentStore:{objectId:'log',length,nativeInputs:states}});
await capture('prefix','inline conversation remains unchanged',{task:{comments:[comment('inline')],unknown:1}});
for(const reference of [null,false,0,''])await capture('prefix','falsy reference '+JSON.stringify(reference),{task:{comments:[comment('inline')],commentStore:reference}});
await capture('prefix','historical prefix reads latest append-only body',{task:task(),rpc:[{value:log([comment('c'),comment('future',{sequence:2})])}]});
await capture('prefix','empty prefix remains empty',{task:task(0,[]),rpc:[{value:log([comment('future')])}]});
await capture('prefix','duplicate snapshot state uses last entry',{task:task(1,[{commentId:'c',handoff:{workflowState:'waiting'}},{commentId:'c',delivery:null,unknown:'last'}]),rpc:[{value:log([comment('c')])}]});
await capture('prefix','missing observation removes current mutable fields',{task:task(1,[]),rpc:[{value:log([comment('c')])}]});
await capture('prefix','lossless state key and arbitrary fields',{task:task(1,[{commentId:'\ud800',nativeInput:{state:'pending'},'\udfff':'raw'}]),rpc:[{value:log([comment('\ud800')])}]});
for(const [name,patch]of [['wrong contract',{contractKey:'other'}],['owned log',{ownerObjectId:'owner'}],['wrong parent',{parentId:'other'}]])await capture('prefix',name,{task:task(),rpc:[{value:log([comment('c')],patch)}]});
for(const length of [-1,1.5,2,'1',9007199254740992])await capture('prefix','invalid prefix '+length,{task:task(length),rpc:[{value:log([comment('c')])}]});
const wrongTask=log([comment('c')]);wrongTask.content.value.taskId='other';await capture('prefix','wrong log task',{task:task(),rpc:[{value:wrongTask}]});
const missingStates=task();delete missingStates.commentStore.nativeInputs;await capture('prefix','missing snapshot states propagate failure',{task:missingStates,rpc:[{value:log([comment('c')])}]});
await capture('prefix','RPC error remains exact',{task:task(),rpc:[{error:{code:'revision_pruned',message:'Original log unavailable. \ud800',outcome:'unknown',details:{pin:'\udfff'}}}]});
const artifact=(id,patch={})=>({object:{objectId:id,revision:2},contentHash:'sha256:'+id,mediaType:'application/json',label:'Evidence '+id,...patch});
const saved=(id,size=7)=>({revision:{contentHash:'sha256:'+id,mediaType:'application/json',byteLength:size}});
await capture('sizes','no findings no evidence reads',{task:{comments:[]}});
await capture('sizes','existing zero and null sizes are preserved',{task:{latestChecks:[{evidence:[artifact('zero',{byteLength:0}),artifact('null',{byteLength:null})]}],comments:[]}});
await capture('sizes','all sources retain original request order',{task:{latestChecks:[{evidence:[artifact('a')]}],content:{checks:[{evidence:[artifact('b')]}]},comments:[{handoff:{checks:[{evidence:[artifact('c')]}]}}]},rpc:['a','b','c'].map(id=>({value:saved(id)}))});
await capture('sizes','equal pins are not deduplicated',{task:{latestChecks:[{evidence:[artifact('a'),artifact('a')]}],comments:[]},rpc:[{value:saved('a')},{value:saved('a')}]});
await capture('sizes','wrong hash refuses retained evidence',{task:{latestChecks:[{evidence:[artifact('a')]}],comments:[]},rpc:[{value:saved('wrong')}]});
const wrongMedia=saved('a');wrongMedia.revision.mediaType='text/plain';await capture('sizes','wrong MIME refuses retained evidence',{task:{latestChecks:[{evidence:[artifact('a')]}],comments:[]},rpc:[{value:wrongMedia}]});
await capture('sizes','evidence error preserves exact metadata',{task:{latestChecks:[{evidence:[artifact('a')]}],comments:[]},rpc:[{error:{code:'revision_pruned',message:'Exact evidence unavailable. \ud800',outcome:'unknown',details:{original:'\udfff'}}}]});
await capture('sizes','non-task record bypasses owned backfill',{key:'task-board/result',task:{content:{checks:[{evidence:[artifact('a')]}]}}});
await capture('sizes','malformed comments fail before earlier evidence reads',{task:{latestChecks:[{evidence:[artifact('a')]}],comments:{}}});
await capture('sizes','null finding fails before earlier evidence reads',{task:{latestChecks:[{evidence:[artifact('a')]}],comments:[{handoff:{checks:[null]}}]}});
await capture('sizes','single artifact flatMap remains supported',{task:{latestChecks:[{evidence:artifact('a')}],comments:[]},rpc:[{value:saved('a')}]});
await capture('sizes','unknown fields and strings remain exact',{task:{'\ud800':{keep:'\udfff'},latestChecks:[{unknown:true,evidence:[artifact('a',{'\ud801':'\ud802'})]}],comments:[]},rpc:[{value:saved('a',19)}]});
const files=['services/task-board/src/runtime/store.ts','services/task-board/src/runtime/comment-store.ts','packages/contracts/src/errors.ts'].map(p=>{const b=fs.readFileSync(path.join(root,p));return{path:p,byteLength:b.length,contentHash:hash(b)};});
const fixture={schemaVersion:1,sourceFiles:files,counts:Object.fromEntries(['sql','prefix','sizes'].map(k=>[k,cases.filter(c=>c.kind===k).length])),cases};
fs.writeFileSync(values.output,JSON.stringify(fixture,null,2)+'\n');
fs.writeFileSync(values.proof,JSON.stringify({at:new Date().toISOString(),node:process.version,sourceFiles:files,moduleConstant:{source:constant[0],value:constant[1]},executed,reused:reusedCount,methods:Object.entries(methods).map(([name,text])=>({name,byteLength:Buffer.byteLength(text),contentHash:hash(Buffer.from(text)),text})),fixture:{byteLength:fs.statSync(values.output).size,contentHash:hash(fs.readFileSync(values.output))},counts:fixture.counts,scope:'Unchanged original failedHandoff SQL and non-tree TaskComments.read; upgradeStored owned backfill with an explicit captured Shared upgradeRecord boundary. No service construction, actual Shared tree traversal or record migration is qualified.'},null,2)+'\n');
console.log(JSON.stringify({counts:fixture.counts,total:cases.length,executed,reused:reusedCount}));
