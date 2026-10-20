// Freeze pure projections from an explicitly supplied original release. Normal
// native tests use the saved observations and never start a deployed service.
import fs from 'node:fs';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
import {parseArgs} from 'node:util';
import assert from 'node:assert/strict';
const {values}=parseArgs({options:{'source-root':{type:'string'},baseline:{type:'string'},output:{type:'string'},proof:{type:'string'}}});for(const key of['source-root','baseline','output','proof'])assert.ok(values[key],key);
const root=resolve(values['source-root']),load=p=>import(pathToFileURL(resolve(root,p))),hash=b=>'sha256:'+createHash('sha256').update(b).digest('hex');
const original=await load('services/task-board/src/runtime/task-read.js'),cards=await load('services/task-board/src/runtime/task-card.js'),comments=await load('services/task-board/src/runtime/comment-store.js'),{openTaskQuestions}=await load('packages/contracts/src/task-board-questions.js'),{hashJson,canonical}=await load('packages/contracts/src/canonical.js');
const RealDate=Date,AT='2026-10-20T12:00:00.000Z';let clockCalls=0;
globalThis.Date=class extends RealDate{constructor(...args){if(!args.length)clockCalls++;super(...(args.length?args:[AT]));}static now(){return RealDate.parse(AT);}};
const clone=structuredClone,baseline=JSON.parse(fs.readFileSync(values.baseline)),cases=[],views=new Map();
const question=(id,sequence,recipient={kind:'user'})=>({commentId:id,sequence,purpose:'question',authorKind:'agent',body:'Question '+id,recipient,replyTo:null,requests:[],responses:[]});
const fields={title:'Fixture',category:null,control:'agent',priority:2,executionRequirement:null,dueAt:null};
const task=patch=>({taskKey:'TASK-0001',fields:clone(fields),workflowState:'todo',claim:null,publication:null,lastRun:null,latestResult:null,acceptedReview:null,deliveries:[],comments:[],commentDeliveries:[],nextSteps:[],consumedSteps:[],runtimeWait:null,...patch});
const step=(id,requires=[])=>({id,scope:'Fixture scope '+id,requires});
const entry=(n,patch={})=>({comment:{commentId:'c'+n,sequence:n,purpose:'update',authorKind:'agent',body:'Entry '+n,replyTo:null,requests:[],responses:[],...patch},receipt:null});
function indexed(entries,summary,patch={}){return{header:{pin:{objectId:'self',revision:7},metadata:{effectivelyArchived:false},value:task({commentStore:{kind:'tree',root:entries.length?{object:{objectId:'tree',revision:3},length:entries.length}:null,length:entries.length},...patch})},entries,summary,query:{}};}
function remember(task){if(!task?.comments)return;const key=hashJson(task.comments),value={key,comments:clone(task.comments),open:openTaskQuestions(task).map(c=>c.commentId)};if(views.has(key))assert.deepEqual(views.get(key),value);else views.set(key,value);}
function error(e){return{error:{code:e.code??e.name,message:e.message,outcome:e.outcome??null,...e.details?{details:e.details}:{}}};}
async function capture(kind,name,input){
 remember(input.header?.value);remember(input.fullTask);remember(input.view?.task);for(const value of Object.values(input.records??{}))if(value.comments)remember(value);
 const trace=[],conditions=[];let active=0,maximum=0;clockCalls=0;
 const port={commentSummary(){trace.push({method:'summary'});return clone(input.summary??undefined);},async read(key,id,...flags){trace.push({method:'read',key,id:clone(id),flags});if(input.failure==='read')throw Object.assign(new Error('Original full read unavailable.'),{code:'reader_unavailable',outcome:'unknown',details:{address:clone(id)}});if(key==='task-board/task'&&typeof id==='object'&&id.objectId==='self')return{value:clone(input.fullTask)};const value=input.records?.[key+':'+(typeof id==='string'?id:id.objectId)];assert.ok(value,'unexpected read '+key+':'+JSON.stringify(id));return{value:clone(value)};},async commentRange(tree,id,start,end){trace.push({method:'range',tree:clone(tree),id,start,end});active++;maximum=Math.max(maximum,active);if(input.stalledStart===start)return await new Promise(()=>{});await Promise.resolve();try{if(input.failureStart===start)throw Object.assign(new Error('Original range unavailable.'),{code:'range_unavailable',outcome:'unknown',details:{start,end}});return clone(input.entries.slice(start,end));}finally{active--;}}};
 const condition=(task,steps)=>{conditions.push({taskHash:hashJson(task),steps:steps??null});return clone(input.condition??steps??{state:'active',detail:'Fixture holds execution precedence.'});};
 let output,outputJson;try{
  if(kind==='read')output=await original.readTask(port,clone(input.header),input.query??{},condition);
  else if(kind==='card')output=cards.taskCard(clone(input.view),input.summary??undefined,input.total??undefined);
  else if(kind==='legacy')output=cards.taskViewPage(clone(input.view),input.query??{});
  else output=comments.commentSummary(clone(input.view.task),input.view.object.objectId);
  if(input.lossless){outputJson=canonical(output);output=undefined;}
 }catch(e){output=error(e);}
 cases.push({kind,name,input,output,outputJson,trace,controls:{clockCalls,conditions,maximumRangeConcurrency:maximum}});
}
const old=baseline.cases.filter(c=>c.kind==='read'),all=[...old.find(c=>c.name==='default descending').output.task.comments].reverse().map(comment=>({comment,receipt:null})),oldSummary={directCount:4,questions:[{commentId:'c2',sequence:2,open:true,recipient:{kind:'user'}},{commentId:'c4',sequence:4,open:true,recipient:{kind:'task',taskId:'parent'}}]};
for(const value of old){remember(value.input.header.value);cases.push({kind:'read',name:value.name,input:{...clone(value.input),entries:clone(all),summary:clone(oldSummary)},output:clone(value.output),trace:value.trace.map(row=>typeof row==='string'?{method:row}:row),reusedObservation:true});}
const entries=[entry(1),entry(2,{...question('c2',2)}),entry(3),entry(4,{...question('c4',4,{kind:'task',taskId:'parent'})}),entry(5),entry(6)];
const summary=clone(oldSummary);
await capture('read','empty indexed prefix preserves empty range',indexed([],{directCount:0,questions:[]}));
await capture('read','empty unresolved selection does not range-read',{...indexed(entries,{directCount:6,questions:[]}),query:{comments:{limit:2,questionsOnly:true}}});
for(const [name,query]of[['before first',{comments:{before:1,limit:2}}],['before beyond append',{comments:{before:99,limit:2}}],['offset beyond end',{comments:{offset:9007199254740991,limit:2}}],['legacy offset with before',{comments:{offset:1,before:2,limit:2}}],['question offset with before',{comments:{offset:0,before:4,limit:2,questionsOnly:true}}],['question cursor retains full total',{comments:{before:4,limit:1,questionsOnly:true}}],['selected user question reused',{comments:{offset:1,limit:1}}]])await capture('read',name,{...indexed(entries,summary),query});
const withReceipts=clone(entries);withReceipts[1].receipt={commentId:'c2',state:'confirmed',source:'first'};withReceipts[3].receipt={commentId:'c4',state:'queued'};
await capture('read','selected receipts reverse with descending comments',{...indexed(withReceipts,summary),query:{comments:{limit:6}}});
await capture('read','archived original header and unprojected fields preserved',indexed(entries,summary,{custom:{keep:'unchanged'}}));
const archived=indexed(entries,summary,{custom:{keep:'unchanged'}});archived.header.metadata.effectivelyArchived=true;await capture('read','archived header is projected exactly',archived);
const full=task({comments:entries.map(e=>e.comment),commentDeliveries:[{commentId:'c2',state:'confirmed',source:'first'},{commentId:'c2',state:'queued',source:'second'}]});
for(const [name,header]of[['missing cached tree summary',indexed(entries,null).header],['retained non-tree reference',{pin:{objectId:'self',revision:2},metadata:{effectivelyArchived:true},value:task({commentStore:{objectId:'old-log',length:6,nativeInputs:[]}})}],['inline header',{pin:{objectId:'self',revision:4},metadata:{effectivelyArchived:false},value:task({comments:entries.map(e=>e.comment)})}]])await capture('read',name,{header,entries,summary:null,fullTask:full,query:{comments:{limit:2}}});
await capture('read','fallback full read failure retains outcome',{header:indexed(entries,null).header,fullTask:full,failure:'read'});
for(const [name,patch]of[['claim',{claim:{phase:'waiting_input',target:{hostId:'fixture-host'},extra:'full-only'}}],['publication',{publication:{operation:{objectId:'op',revision:3}}}]])for(const view of[undefined,'card'])await capture('read',name+' skips assessment and clock '+(view??'full'),{...indexed(entries,summary,{...patch,nextSteps:[step('dependency',[{kind:'task',taskId:'unavailable',until:'idle'}])]}),query:{...view?{view}:{},comments:{limit:2}}});
const closed=task({comments:[question('q',1,{kind:'task',taskId:'parent'}),{commentId:'answer',sequence:2,purpose:'update',authorKind:'agent',sourceTaskId:'parent',replyTo:'q',body:'Addressed answer',requests:[],responses:[]}],nextSteps:[step('question',[{kind:'question',questionId:'q'}])]});
await capture('read','fallback summary retains referenced closed question',{header:indexed([],null).header,fullTask:closed,query:{comments:{limit:1}}});
await capture('read','indexed closed question avoids full conversation',{...indexed(entries,{directCount:4,questions:[{commentId:'q',sequence:1,open:false,recipient:{kind:'task',taskId:'parent'}}]},{nextSteps:closed.nextSteps}),query:{view:'card'}});
await capture('read','incomplete index preserves question mismatch',{...indexed(entries,{directCount:4,questions:[]},{nextSteps:closed.nextSteps}),query:{view:'card'}});
await capture('read','foreign delivery assessment uses retained header',{...indexed(entries,summary,{nextSteps:[step('delivery',[{kind:'delivery',taskId:'other',key:'api'}])]}),records:{'task-board/task:other':task({deliveries:[{identity:'sha256:'+'b'.repeat(64),delivery:{key:'api',artifacts:[],codeReferences:[]}}]})},query:{view:'card'}});
await capture('read','next appointment uses supplied clock',{...indexed(entries,summary,{nextSteps:[step('time',[{kind:'time',at:'2026-10-20T13:00:00.000Z'}])]}),query:{view:'card'}});
const many=Array.from({length:112},(_,i)=>entry(i+1,i<111?question('c'+(i+1),i+1):{})),manySummary={directCount:112,questions:many.slice(0,111).map(e=>({commentId:e.comment.commentId,sequence:e.comment.sequence,open:true,recipient:{kind:'user'}}))};
await capture('read','last100 open users are fetched in batches of8',{...indexed(many,manySummary),query:{comments:{limit:1}}});
await capture('read','question selection batches with available error',{...indexed(many,manySummary),query:{comments:{limit:8,questionsOnly:true}},failureStart:103,stalledStart:104});
const view={object:{objectId:'self',revision:7},task:full,executionCondition:{state:'idle',detail:'Original view'},archived:false,custom:{preserve:'view'}};
for(const query of[{}, {comments:{limit:2}}, {comments:{offset:1,limit:2}}, {comments:{questionsOnly:true,limit:1}}, {comments:{offset:99,limit:2}}])await capture('legacy','retained page '+JSON.stringify(query),{view,query});
const cross=task({comments:[entry(1).comment,{...entry(2).comment,sourceTaskId:'other'},{...entry(3).comment,sourceTaskId:'self'},entry(4,{recipient:{kind:'task',taskId:'parent'}}).comment,entry(5,{questionSource:{taskId:'other',commentId:'q'}}).comment,entry(6,{authorKind:'user'}).comment,entry(7,{authorKind:'system'}).comment,question('q',8)]});
await capture('card','card without summary counts actual direct comments',{view:{...view,task:cross}});
await capture('card','provided summary avoids conversation classification',{view:{...view,task:cross},summary:{directCount:23,questions:[{commentId:'u',open:true},{commentId:'p',open:true,recipient:{kind:'task',taskId:'parent'}},{commentId:'closed',open:false}]},total:300});
await capture('summary','summary keeps closed references and excludes foreign question source',{view:{...view,task:{...closed,comments:[...closed.comments,{...question('foreign',3),sourceTaskId:'other'},question('open',4)]}}});
const lossless=indexed([entry(1,{body:'\udfff'})],{directCount:1,questions:[]},{fields:{...fields,title:'\ud800'}});await capture('read','lossless Task and comment strings',{...lossless,lossless:true});
const cursorBefore={...indexed(entries,summary),query:{comments:{before:5,limit:2}}};await capture('read','sequence cursor before append',cursorBefore);
const cursorAfter={...indexed([...entries,entry(7)],{...summary,directCount:summary.directCount+1}),query:cursorBefore.query};cursorAfter.header.pin.revision=8;cursorAfter.header.value.commentStore.root.object={objectId:'tree-after',revision:4};await capture('read','sequence cursor after append',cursorAfter);
const pins=['services/task-board/src/runtime/task-read.js','services/task-board/src/runtime/task-card.js','services/task-board/src/runtime/comment-store.js','services/task-board/src/runtime/conversation.js','packages/contracts/src/task-board-questions.js'].map(path=>{const bytes=fs.readFileSync(resolve(root,path));return{path,byteLength:bytes.length,contentHash:hash(bytes)};});
const fixture={schemaVersion:1,at:AT,sourceFiles:pins,reusedCases:7,newCases:cases.length-7,questionViews:[...views.values()],cases};fs.writeFileSync(values.output,JSON.stringify(fixture,null,2)+'\n');fs.writeFileSync(values.proof,JSON.stringify({at:new RealDate().toISOString(),node:process.version,nodeHash:hash(fs.readFileSync(process.execPath)),sourceFiles:pins,reusedBaselineHash:hash(fs.readFileSync(values.baseline)),fixture:{byteLength:fs.statSync(values.output).size,contentHash:hash(fs.readFileSync(values.output))},newObservations:cases.filter(c=>!c.reusedObservation)},null,2)+'\n');console.log(JSON.stringify({reused:7,newCases:fixture.newCases,bytes:fs.statSync(values.output).size,questionViews:views.size}));
