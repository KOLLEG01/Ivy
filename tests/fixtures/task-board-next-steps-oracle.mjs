// Capture against an explicitly supplied, verified original release. Normal Rust
// tests consume the frozen observations and never require that deployment.
import fs from 'node:fs';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
import {parseArgs} from 'node:util';
import assert from 'node:assert/strict';
const {values}=parseArgs({options:{'source-root':{type:'string'},baseline:{type:'string'},output:{type:'string'},proof:{type:'string'}}});
for(const key of['source-root','baseline','output','proof'])assert.ok(values[key],key);
const root=resolve(values['source-root']),load=p=>import(pathToFileURL(resolve(root,p))),hash=b=>'sha256:'+createHash('sha256').update(b).digest('hex');
const original=await load('services/task-board/src/runtime/next-steps.js'),{openTaskQuestions}=await load('packages/contracts/src/task-board-questions.js'),{hashJson}=await load('packages/contracts/src/canonical.js');
const baseline=JSON.parse(fs.readFileSync(values.baseline)),AT='2026-10-20T12:00:00.000Z',FUTURE='2026-10-20T13:00:00.000Z',clone=structuredClone;
const task=patch=>({taskKey:'TASK-0001',workflowState:'todo',claim:null,publication:null,lastRun:null,latestResult:null,acceptedReview:null,deliveries:[],comments:[],commentDeliveries:[],nextSteps:[],consumedSteps:[],runtimeWait:null,...patch});
const step=(id,requires=[])=>({id,scope:'Original bounded scope '+id,requires}),cond=(id='other',until='done')=>({kind:'task',taskId:id,until}),question=(id,recipient)=>({commentId:id,sequence:1,purpose:'question',authorKind:'agent',body:'Question',replyTo:null,requests:[],responses:[],...recipient?{recipient}:{}});
const questionViews=new Map(),cases=[],full=[];
function remember(value){if(!value?.comments)return;const comments=clone(value.comments),key=hashJson(comments);const open=openTaskQuestions(value).map(c=>c.commentId);if(questionViews.has(key))assert.deepEqual(questionViews.get(key).open,open);else questionViews.set(key,{key,comments,open});}
function chain(n){const records={};for(let i=1;i<=n;i++)records['task-board/task:n'+i]=task({nextSteps:i===n?[]:[step('next',[cond('n'+(i+1))])]});return records;}
function store(input){const records=input.chainLength?chain(input.chainLength):input.records??{},trace=[];for(const value of Object.values(records))remember(value);return{trace,async read(key,id,...flags){trace.push({method:'read',key,id:clone(id),flags});const address=typeof id==='string'?id:id.objectId;if(input.stalled===address)return await new Promise(()=>{});if(input.failure===address){const e=Object.assign(new Error('Original read unavailable.'),{code:'owner_unavailable',outcome:'unknown',details:{address}});throw e;}assert.ok(records[key+':'+address],key+':'+address);return{value:clone(records[key+':'+address])};}};}
function result(error){return{error:{code:error.code??error.name,message:error.message,outcome:error.outcome??null,...error.details?{details:error.details}:{}}};}
async function capture(kind,name,input){const port=store(input);remember(input.task);let output;try{if(kind==='assessment'){const values=await original.assessSteps(port,input.task,AT,new Map(Object.entries(input.overrides??{})),input.index??undefined);output={assessments:values,condition:original.stepCondition(values)};}else {await original.validateNextSteps(port,input.own??null,input.steps,input.task??undefined,AT);output=null;}}catch(e){output=result(e);}const row={kind,name,input,output,trace:port.trace};full.push(row);cases.push(input.chainLength?{...row,trace:undefined,traceCount:port.trace.length,traceHash:hashJson(port.trace)}:row);}
for(const c of baseline.cases.filter(c=>c.kind!=='read')){
 let input=clone(c.input),output=clone(c.output),kind=c.kind;
 if(kind==='assessment'){output={assessments:output,condition:original.stepCondition(output)};remember(input.task);}
 if(kind==='validate'){input={...input,own:'self',task:task({nextSteps:input.steps})};remember(input.task);}
 if(kind==='consume'){input=input.subject?{task:input.subject,selected:input.selected}:{task:input,selected:{step:{id:'new'},fingerprint:output.at(-1).fingerprint}};}
 if(kind==='time')input={task:input};
 for(const value of Object.values(input.records??{}))remember(value);
 cases.push({...c,kind,input,output,reusedObservation:true});
}
await capture('assessment','missing result/review stay absent in fingerprint',{task:task({nextSteps:[step('idle',[cond('other','idle')])]}),records:{'task-board/task:other':{workflowState:'todo',claim:null,publication:null,lastRun:null}}});
await capture('assessment','overrides avoid header reads but preserve Run pins',{task:task({nextSteps:[step('a',[cond('other','idle')]),step('b',[cond('other','done')])]}),overrides:{other:task({workflowState:'done',lastRun:{objectId:'r',revision:9}})},records:{'task-board/run:r':{phase:'running'}}});
await capture('assessment','shared header with separate saved Run reads',{task:task({nextSteps:[step('a',[cond('other','idle')]),step('b',[cond('other','idle')])]}),records:{'task-board/task:other':task({lastRun:{objectId:'r',revision:9}}),'task-board/run:r':{phase:'completed'}}});
for(const [name,patch]of[['claim',{claim:{run:'r'}}],['publication',{publication:{operation:'p'}}]])await capture('assessment',name+' prevents idle',{task:task({nextSteps:[step('idle',[cond('other','idle')])]}),records:{'task-board/task:other':task(patch)}});
await capture('assessment','done ignores claim but still loads Run',{task:task({nextSteps:[step('done',[cond()])]}),records:{'task-board/task:other':task({workflowState:'done',claim:{run:'r'},lastRun:{objectId:'r',revision:2}}),'task-board/run:r':{phase:'running'}}});
for(const [name,recipient,kind,index]of[['missing recipient',null,'question',null],['legacy user alias',{kind:'task',taskId:'parent'},'user',null],['indexed open task',{kind:'task',taskId:'parent'},'question',[{commentId:'q',sequence:1,open:true,recipient:{kind:'task',taskId:'parent'}}]]])await capture('assessment',name+' classification',{task:task({comments:[question('q',recipient)],nextSteps:[step('q',[{kind,questionId:'q'}])]}),index});
await capture('assessment','empty steps need workflow attention',{task:task()});
await capture('assessment','read error retains outcome and starts independent read',{task:task({nextSteps:[step('a',[cond('bad')]),step('b',[cond('waiting')])]}),failure:'bad',stalled:'waiting'});
await capture('assessment','Run read error retains exact pin and outcome',{task:task({nextSteps:[step('idle',[cond('other','idle')])]}),records:{'task-board/task:other':task({lastRun:{objectId:'bad',revision:9}})},failure:'bad'});
for(const [name,steps]of[
 ['missing id',[{scope:'Missing identity',requires:[]}]],['missing requires',[{id:'bad',scope:'Missing prerequisites'}]],['unknown property',[{...step('bad'),extra:true}]],['empty scope',[{...step('bad'),scope:''}]],['unknown condition',[step('bad',[{kind:'unknown'}])]],
 ...['2026-10-20T13:00:00Z','2026-10-20T13:00:00.0Z','2026-02-29T00:00:00.000Z','2026-10-20T13:00:00+00:00','+010000-01-01T00:00:00.000Z'].map(at=>['time '+at,[step('time',[{kind:'time',at}])]])
])await capture('validate',name,{steps,task:task()});
await capture('validate','question belongs to another ticket',{steps:[step('q',[{kind:'question',questionId:'foreign'}])],task:task({comments:[{...question('foreign',{kind:'user'}),sourceTaskId:'other'}]})});
for(const state of['done','cancelled','review'])await capture('validate','terminal foreign '+state+' breaks cycle',{own:'self',steps:[step('a',[cond()])],task:task(),records:{'task-board/task:other':task({workflowState:state,nextSteps:[step('back',[cond('self')])]})}});
await capture('validate','claimed foreign task breaks cycle',{own:'self',steps:[step('a',[cond()])],task:task(),records:{'task-board/task:other':task({claim:{run:'r'},nextSteps:[step('back',[cond('self')])]})}});
await capture('validate','foreign loop without this ticket permits validation',{own:'self',steps:[step('a',[cond('a')])],task:task(),records:{'task-board/task:a':task({nextSteps:[step('ab',[cond('b')])]}),'task-board/task:b':task({nextSteps:[step('ba',[cond('a')])]})}});
for(const n of[1023,1024])await capture('validate','graph chain bound '+n,{own:'self',steps:[step('a',[cond('n1')])],task:task(),chainLength:n});
const files=['services/task-board/src/runtime/next-steps.js','packages/contracts/src/task-board-questions.js'].map(path=>{const b=fs.readFileSync(resolve(root,path));return{path,byteLength:b.length,contentHash:hash(b)};});
const losslessTask={comments:[],nextSteps:[{id:'\ud800',scope:'\udfff',requires:[]}]};
const losslessValues=await original.assessSteps(store({}),losslessTask,AT);
const losslessReference={taskJson:JSON.stringify(losslessTask),fingerprint:losslessValues[0].fingerprint};
const fixture={schemaVersion:1,at:AT,sourceFiles:files,reusedCases:26,newCases:full.length,questionViews:[...questionViews.values()],losslessReference,cases};assert.equal(cases.filter(c=>c.reusedObservation).length,26);
fs.writeFileSync(values.output,JSON.stringify(fixture,null,2)+'\n');fs.writeFileSync(values.proof,JSON.stringify({at:new Date().toISOString(),node:process.version,nodeHash:hash(fs.readFileSync(process.execPath)),sourceFiles:files,reusedBaselineHash:hash(fs.readFileSync(values.baseline)),reusedCases:26,newCases:full.length,fullNewObservations:full,fixture:{contentHash:hash(fs.readFileSync(values.output)),byteLength:fs.statSync(values.output).size}},null,2)+'\n');console.log(JSON.stringify({reusedCases:26,newCases:full.length,fixtureBytes:fs.statSync(values.output).size,questionViews:questionViews.size}));
