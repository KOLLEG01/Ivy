// Capture pure current projections from the supplied immutable original release.
// Resolver views are source inputs for the Rust bridge, not native resolver proof.
import fs from 'node:fs';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {parseArgs} from 'node:util';
import {createHash} from 'node:crypto';
import assert from 'node:assert/strict';
const {values}=parseArgs({options:{'source-root':{type:'string'},output:{type:'string'},proof:{type:'string'}}});
for(const key of ['source-root','output','proof'])assert.ok(values[key],key);
const root=resolve(values['source-root']),load=p=>import(pathToFileURL(resolve(root,p)));
const hash=b=>'sha256:'+createHash('sha256').update(b).digest('hex');
const conversation=await load('services/task-board/src/runtime/conversation.js');
const {openTaskQuestions}=await load('packages/contracts/src/task-board-questions.js');
const {commentSummary}=await load('services/task-board/src/runtime/comment-store.js');
const {assessSteps,stepCondition}=await load('services/task-board/src/runtime/next-steps.js');
const {TaskBoardEngine}=await load('services/task-board/src/runtime/engine.js');
const {canonical}=await load('packages/contracts/src/canonical.js');
const clone=structuredClone,AT='2026-10-20T12:00:00.000Z',cases=[];
let sequence=0;
const comment=patch=>({commentId:'c'+(++sequence),sequence,authorKind:'agent',purpose:'update',body:'Fixture',requests:[],responses:[],replyTo:null,...patch});
const task=patch=>({taskKey:'TASK-0001',workflowState:'todo',fields:{title:'Fixture',category:null,control:'agent',priority:2,executionRequirement:null,dueAt:null},comments:[],commentDeliveries:[],nextSteps:[],consumedSteps:[],claim:null,publication:null,runtimeWait:null,completionIssue:null,...patch});
const question=patch=>comment({purpose:'question',...patch});
const native=(state='pending')=>({state,serviceNodeId:'agent',requestId:'input',connectionEpoch:'epoch',inputToken:'token'});
const store={async read(){throw Error('Unexpected foreign/full-record read.');},failedHandoff(){throw Error('Unexpected retained handoff read.');}};
async function capture(kind,name,input){
 const before=JSON.stringify(input);let output,resolverViews;
 if(kind==='cross')output=conversation.crossAgentComment(input.comment,input.taskId);
 else if(kind==='unread')output=conversation.unreadComments(input.task,input.taskId,input.state);
 else if(kind==='handoff')output=conversation.runHandoff(input.task,input.runId)??null;
 else if(kind==='queuedUser')output=conversation.queuedUserInput(input.task);
 else if(kind==='queuedEdits')output=conversation.queuedTaskEdits(input.task);
 else if(kind==='questions'){
   resolverViews={all:openTaskQuestions(input.task),run:openTaskQuestions(input.task,input.runId)};
   output={openIds:resolverViews.all.map(q=>q.commentId),unanswered:conversation.unansweredQuestion(input.task,input.runId)};
 }else{
   resolverViews={all:openTaskQuestions(input.task)};
   const summary=commentSummary(input.task,input.taskId),assessments=await assessSteps(store,input.task,AT),steps=stepCondition(assessments);
   output={summary,assessments,condition:TaskBoardEngine.prototype.condition.call({store},input.task,steps)};
 }
 assert.equal(JSON.stringify(input),before,name+' original input changed');
 cases.push({kind,name,input,output,...resolverViews?{resolverViews}: {}});
}
for(const [name,patch] of [
 ['own agent',{}],['own source',{sourceTaskId:'self'}],['foreign source',{sourceTaskId:'other'}],
 ['empty source',{sourceTaskId:''}],['foreign source on user',{authorKind:'user',sourceTaskId:'other'}],
 ['task recipient own',{recipient:{kind:'task',taskId:'self'}}],['user recipient foreign',{recipient:{kind:'user'},sourceTaskId:'other'}],
 ['mirrored question on user',{authorKind:'user',questionSource:{taskId:'other',commentId:'q'}}],
 ['null mirrored source',{questionSource:null}],['empty object mirrored source',{questionSource:{}}],
 ['lossless exact source',{sourceTaskId:'self-\ud800'}],['lossless different source',{sourceTaskId:'self-\udfff'}]
])await capture('cross',name,{comment:comment(patch),taskId:name.startsWith('lossless')?'self-\ud800':'self'});

const mixed=task({comments:[comment({authorKind:'user'}),comment({}),comment({sourceTaskId:'other'}),comment({authorKind:'system'}),comment({sourceTaskId:'self'}),comment({recipient:{kind:'task',taskId:'other'}}),comment({questionSource:{taskId:'other'}}),comment({})]});
for(const [name,state]of [
 ['unread all',undefined],['explicit zero',{readAgentCommentCount:0,readDirectAgentCommentCount:0}],
 ['legacy first direct',{readAgentCommentCount:1}],['legacy includes cross and self',{readAgentCommentCount:3}],
 ['legacy whole prefix',{readAgentCommentCount:99}],['direct cursor independent',{readDirectAgentCommentCount:2}],
 ['direct cursor exceeds total',{readDirectAgentCommentCount:99}],['max of both cursors',{readAgentCommentCount:3,readDirectAgentCommentCount:1}],
 ['cross unread after direct cursor',{readAgentCommentCount:2,readDirectAgentCommentCount:99}]
])await capture('unread',name,{task:clone(mixed),taskId:'self',...state?{state}:{}});
await capture('unread','empty comments',{task:task({}),taskId:'self',state:{readAgentCommentCount:1}});
await capture('unread','no agent comments',{task:task({comments:[comment({authorKind:'user'}),comment({authorKind:'system'})]}),taskId:'self'});

await capture('handoff','no handoff',{task:task({comments:[comment({})]}),runId:'run'});
await capture('handoff','exact run excludes others',{task:task({comments:[comment({purpose:'handoff',run:{objectId:'other',revision:1}}),comment({run:{objectId:'run',revision:1}})]}),runId:'run'});
await capture('handoff','last array entry wins across revision and author',{task:task({comments:[comment({purpose:'handoff',run:{objectId:'run',revision:9}}),comment({purpose:'handoff',authorKind:'user',sequence:1,run:{objectId:'run',revision:1}})]}),runId:'run'});
await capture('handoff','lossless handoff run',{task:task({comments:[comment({purpose:'handoff',run:{objectId:'run-\ud800',revision:1}})]}),runId:'run-\ud800'});

const user=comment({authorKind:'user'}),agent=comment({}),edit=comment({authorKind:'system',body:'Ticket edited'}),fake=comment({authorKind:'agent',body:'Ticket edited'}),inputEdit=comment({authorKind:'system',body:'Ticket edited',nativeInput:native()}),lower=comment({authorKind:'system',body:'ticket edited'});
for(const [name,comments,deliveries]of [
 ['empty',[],[]],['queued user',[user],[{commentId:user.commentId,state:'queued'}]],
 ['delivered user',[user],[{commentId:user.commentId,state:'delivered'}]],['queued agent',[agent],[{commentId:agent.commentId,state:'queued'}]],
 ['missing user comment',[user],[{commentId:'missing',state:'queued'}]],
 ['mixed user queue',[user,agent],[{commentId:agent.commentId,state:'queued'},{commentId:user.commentId,state:'queued'}]]
])await capture('queuedUser',name,{task:task({comments,commentDeliveries:deliveries})});
for(const [name,comments,deliveries]of [
 ['empty',[],[]],['exact edit',[edit],[{commentId:edit.commentId,state:'queued'}]],
 ['delivered edit',[edit],[{commentId:edit.commentId,state:'delivered'}]],['agent text is not edit',[fake],[{commentId:fake.commentId,state:'queued'}]],
 ['native marker excluded',[inputEdit],[{commentId:inputEdit.commentId,state:'queued'}]],['case sensitive edit',[lower],[{commentId:lower.commentId,state:'queued'}]],
 ['missing edit comment',[edit],[{commentId:'missing',state:'queued'}]],
 ['delivery order and duplicates',[edit,lower],[{commentId:edit.commentId,state:'queued'},{commentId:lower.commentId,state:'queued'},{commentId:edit.commentId,state:'queued'}]],
 ['null native marker',[{...edit,nativeInput:null}],[{commentId:edit.commentId,state:'queued'}]]
])await capture('queuedEdits',name,{task:task({comments,commentDeliveries:deliveries})});

const q=question({recipient:{kind:'user'},run:{objectId:'run',revision:1}});
const tq=question({recipient:{kind:'task',taskId:'parent'}});
const nq=question({nativeInput:native(),run:{objectId:'run',revision:1}});
const rq=question({requests:[{requestId:'r1'},{requestId:'r2'}]});
const answer=(target,patch={})=>comment({authorKind:'user',replyTo:target.commentId,...patch});
for(const [name,comments,runId]of [
 ['empty',[],'run'],['own user question',[q],'run'],['foreign source question',[{...q,sourceTaskId:'other'}],'run'],
 ['user addressed answer',[q,answer(q)],'run'],['worker answer does not close user question',[q,answer(q,{authorKind:'agent'})],'run'],
 ['unrelated handoff does not answer',[q,comment({purpose:'handoff',authorKind:'agent'})],'run'],
 ['earlier reply cannot close later question',[answer(q),q],'run'],
 ['explicit resolution closes',[q,comment({resolvesQuestions:[q.commentId]})],'run'],
 ['task recipient remains open on user reply',[tq,answer(tq)],'run'],
 ['task recipient exact source reply',[tq,answer(tq,{authorKind:'agent',sourceTaskId:'parent'})],'run'],
 ['wrong source does not close task question',[tq,answer(tq,{authorKind:'agent',sourceTaskId:'other'})],'run'],
 ['requests all answered across later comments',[rq,comment({responses:[{requestId:'r1'}]}),comment({responses:[{requestId:'r2'}]})],'run'],
 ['partial request coverage remains open',[rq,comment({responses:[{requestId:'r1'}]})],'run'],
 ['native same run open',[nq],'run'],['native other run filtered',[nq],'other'],['empty run does not filter',[nq],''],
 ['native addressed answer',[nq,answer(nq,{nativeInput:native('answered')})],'run'],
 ['native expiration',[nq,answer(nq,{authorKind:'system',nativeInput:native('expired')})],'run'],
 ['native plain user reply is not native completion',[nq,answer(nq)],'run'],
 ['native wrong state remains open',[nq,answer(nq,{nativeInput:native('pending')})],'run'],
 ['native explicit resolution alone stays open',[nq,comment({resolvesQuestions:[nq.commentId]})],'run'],
 ['normal question survives different run filter',[q],'other'],
 ['lossless question and run identity',[{...nq,commentId:'q-\udfff',run:{objectId:'run-\ud800',revision:1}}],'run-\ud800']
])await capture('questions',name,{task:task({comments}),runId});

for(const [name,comments,questionId,independent]of [
 ['open local user gate',[q],q.commentId,false],['answered local user gate',[q,answer(q)],q.commentId,false],
 ['open addressed task gate',[tq],tq.commentId,false],['answered task gate',[tq,answer(tq,{authorKind:'agent',sourceTaskId:'parent'})],tq.commentId,false],
 ['independent step with open question',[q],q.commentId,true]
])await capture('bridge',name,{task:task({comments,nextSteps:[{id:'question',scope:'Address this question.',requires:[{kind:'question',questionId}]},...independent?[{id:'independent',scope:'Independent authorized work.',requires:[]}]:[]]}),taskId:'self'});

const sourceFiles=['services/task-board/src/runtime/conversation.js','packages/contracts/src/task-board-questions.js','services/task-board/src/runtime/comment-store.js','services/task-board/src/runtime/next-steps.js','services/task-board/src/runtime/engine.js'].map(path=>{const b=fs.readFileSync(resolve(root,path));return{path,byteLength:b.length,contentHash:hash(b)};});
const fixture={schemaVersion:1,at:AT,sourceFiles,cases};
fs.writeFileSync(values.output,JSON.stringify(fixture,null,2)+'\n');
const wire=JSON.parse(fs.readFileSync(values.output));
const counts=Object.fromEntries([...new Set(cases.map(c=>c.kind))].map(kind=>[kind,cases.filter(c=>c.kind===kind).length]));
fs.writeFileSync(values.proof,JSON.stringify({at:new Date().toISOString(),node:process.version,nodeHash:hash(fs.readFileSync(process.execPath)),sourceFiles,counts,cases:cases.length,fixture:{byteLength:fs.statSync(values.output).size,contentHash:hash(fs.readFileSync(values.output))},losslessCanonicalHash:hash(Buffer.from(canonical(wire))),scope:'Actual unchanged pure original conversation functions, resolver views and finite next-step/summary/condition composition. Rust consumes captured resolver views as explicit source inputs; native Shared resolver behavior/binding is not qualified. No construction, persistent writes, scheduler, native dispatch or live effects.'},null,2)+'\n');
console.log(JSON.stringify({cases:cases.length,counts,nativeResolverQualification:false}));
