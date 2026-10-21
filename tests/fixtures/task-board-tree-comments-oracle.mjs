// Capture the unchanged frozen TaskComments class with its actual Shared reader,
// canonical hash and question resolver. The only supplied boundary is objects.read.
import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import {stripTypeScriptTypes} from 'node:module';
import {createHash} from 'node:crypto';
import {parseArgs} from 'node:util';
import assert from 'node:assert/strict';
const {values}=parseArgs({options:{'source-root':{type:'string'},'release-root':{type:'string'},output:{type:'string'},proof:{type:'string'}}});
for(const key of ['source-root','release-root','output','proof'])assert.ok(values[key],key);
const source=path.resolve(values['source-root']),release=path.resolve(values['release-root']);
const files=['services/task-board/src/runtime/comment-store.ts','packages/sdk/src/task-comments.ts'];
const originals=files.map(file=>{const b=fs.readFileSync(path.join(source,file));return{path:file,byteLength:b.length,contentHash:'sha256:'+createHash('sha256').update(b).digest('hex'),text:b.toString('utf8')};});
assert.equal(originals[0].contentHash,'sha256:730bf3cb0d04bbdf993ed79110cbd7cccdaefdc9615b8280f828d8b897f8a5d9');
assert.equal(originals[1].contentHash,'sha256:eda30de9d2741fcf4cc19088861a57e2fbc8cd4d5294e1afa3e27e2da6aa47be');
const sdk=await import(pathToFileURL(path.join(release,'packages/sdk/src/node.js')));
const {crossAgentComment,openQuestions}=await import(pathToFileURL(path.join(release,'services/task-board/src/runtime/conversation.js')));
const evaluate=(text,names,bindings)=>new Function(...Object.keys(bindings),stripTypeScriptTypes(text.replace(/^import .*;\r?\n/gm,'').replace(/^export /gm,''),{mode:'transform'})+'\nreturn {'+names.join(',')+'};')(...Object.values(bindings));
const {CommentReader,commentNodeKey}=evaluate(originals[1].text,['CommentReader','commentNodeKey'],{});
const {TaskComments,commentSummary}=evaluate(originals[0].text,['TaskComments','commentSummary'],{...sdk,CommentReader,commentNodeKey,crossAgentComment,openQuestions});
const clone=structuredClone,base={comments:[],commentDeliveries:[],nextSteps:[],unknown:{keep:'\ud800😀'}};
const pin=(id,revision=1)=>({objectId:id,revision});
const leaf=(id,sequence=1,patch={})=>({schemaVersion:1,taskId:'task',kind:'comment',entry:{comment:{commentId:id,sequence,authorKind:'agent',purpose:'update',body:'\ud800😀',requests:[],responses:[],...patch},receipt:null}});
const tree=(id='root',length=1,patch={})=>({kind:'tree',root:pin(id)?{object:pin(id),length}:null,length,...patch});
const task=(treeValue,patch={})=>({...clone(base),commentStore:treeValue,...patch});
const cases=[],add=(name,nodes,actions,extra={})=>cases.push({name,input:{nodes,actions,...extra}});
const read=value=>({kind:'read',task:value}),summary=(value,taskId='task')=>({kind:'summary',task:value,taskId}),node=(id,revision=1)=>({kind:'node',pin:pin(id,revision)}),lookup=value=>({kind:'pin',node:value});
const a=leaf('a'),b=leaf('b',2,{sourceTaskId:'other'});b.entry.receipt={commentId:'b',state:'queued'};
const page={schemaVersion:1,taskId:'task',kind:'page',children:[{object:pin('a'),length:1},{object:pin('b'),length:1}]};
add('full tree hydrates exact comments receipts and unknown header',{root:page,a,b},[read(task(tree('root',2))),lookup(page),lookup(a),lookup(b)]);
add('summary is computed once and keyed by referenced questions',{root:a},[summary(task(tree())),read(task(tree())),summary(task(tree())),summary(task(tree(),{nextSteps:[{requires:[{kind:'question',questionId:'different'}]}]}))]);
add('range uses intersecting nodes and the same pin observer',{root:page,a,b},[{kind:'range',tree:tree('root',2),start:1,end:2},lookup(page),lookup(a),lookup(b)]);
add('embedded summary bypasses key and recomputation',{root:a},[summary(task(tree('root',1,{summary:{directCount:91,questions:[]}}),{nextSteps:null})),read(task(tree('root',1,{summary:{directCount:91,questions:[]}}),{nextSteps:null}))]);
for(const embedded of [false,0,''])add('non-nullish summary '+JSON.stringify(embedded),{},[summary(task(tree('root',1,{summary:embedded}),{nextSteps:null}))]);
const referenced={...a,entry:{...a.entry,comment:{...a.entry.comment,purpose:'question',recipient:{kind:'user'}}}};
const steps=ids=>ids.map(id=>({requires:[{kind:'question',questionId:id}]}));
add('summary keys deduplicate and sort UTF16 gate identifiers',{root:referenced},[read(task(tree(),{nextSteps:steps(['a','z','\ud800'])})),summary(task(tree(),{nextSteps:steps(['\ud800','z','a','a'])})),summary(task(tree(),{nextSteps:steps(['a'])}))]);
add('different immutable revision retains distinct observed pins',{'root@1':a,'root@2':leaf('new')},[node('root',1),node('root',2),lookup(a),lookup(leaf('new'))]);
add('cached node checks task before observation',{root:a},[node('root'),{...node('root'),taskId:'other'},lookup(a)]);
add('raw request object keeps identity and lossless fields',{},[node('root')],{error:{name:'CustomError',message:'raw\ud800',code:'not_found',details:{x:'\udfff'}}});
add('raw primitive request remains a primitive',{},[node('root')],{error:'raw\ud800'});
add('raw metadata Error is not converted to IvyError',{root:a},[node('root'),lookup(a)],{metadata:{ownerObjectId:'other'}});
add('raw metadata TypeError is not converted to IvyError',{root:a},[node('root')],{missingObject:true});
for(const size of [65536,65537]){
 const question=leaf('',1,{purpose:'question',recipient:{kind:'user'}});
 const overhead=Buffer.byteLength(JSON.stringify(commentSummary({...clone(base),comments:[question.entry.comment]},'task')));
 question.entry.comment.commentId='x'.repeat(size-overhead);
 assert.equal(Buffer.byteLength(JSON.stringify(commentSummary({...clone(base),comments:[question.entry.comment]},'task'))),size);
 add('summary UTF8 capacity '+size,{root:question},[read(task(tree())),summary(task(tree()))]);
}
add('summary cache retains 128 keys and replacement eviction order',{},[{kind:'summaryCapacity'}]);
add('publication pin FIFO observes cached node returns at capacity',{},[{kind:'pinCapacity'}],{generated:true});
for(const item of cases){
 const input=item.input,before=JSON.stringify(input),trace=[],raw=input.error,client={async request(method,params){
  assert.equal(method,'objects.read');trace.push({method,params:clone(params)});
  if('error'in input)throw raw;
  const id=params.objectId,node=input.generated?leaf(id):input.nodes[id+'@'+params.revision]??input.nodes[id];assert.ok(node,'Unexpected source pin '+id);
  return {...input.missingObject?{}:{object:{contractKey:commentNodeKey,parentId:'task',ownerObjectId:'task',...input.metadata}},revision:{contractVersion:'1.0.0',revision:params.revision},content:{encoding:'json',value:clone(node)}};
 }};
 const comments=new TaskComments(client,async()=>{throw Error('No identity allocation is allowed');});
 const output=[];
 for(const action of input.actions){
  try{
   const id=action.taskId??'task';let value;
   if(action.kind==='read')value=await comments.read(clone(action.task),id);
   else if(action.kind==='summary')value=comments.summary(action.task,action.task.commentStore,id)??null;
   else if(action.kind==='range')value=await comments.range(action.tree,id,action.start,action.end);
   else if(action.kind==='node')value=await comments.reader.node(action.pin,id);
   else if(action.kind==='pin')value=comments.pins.get(sdk.hashJson(action.node))??null;
   else if(action.kind==='summaryCapacity'){
    const empty={kind:'tree',root:null,length:0},v=task(empty);
    for(let i=0;i<128;i++)await comments.read(v,'t'+i);
    await comments.read(v,'t127');
    const first=comments.summary(v,empty,'t0')??null,second=comments.summary(v,empty,'t1')??null;
    await comments.read(v,'t0');
    value={first,second,restored:comments.summary(v,empty,'t0')??null,secondAfter:comments.summary(v,empty,'t1')??null};
   }else{
    for(let i=0;i<16384;i++)await comments.reader.node(pin('n'+i),'task');
    await comments.reader.node(pin('n16383'),'task');
    await comments.reader.node(pin('n16384'),'task');
    const first=comments.pins.get(sdk.hashJson(leaf('n0')))??null,second=comments.pins.get(sdk.hashJson(leaf('n1')))??null;
    await comments.reader.node(pin('n0'),'task');
    value={first,second,restored:comments.pins.get(sdk.hashJson(leaf('n0')))??null,secondAfter:comments.pins.get(sdk.hashJson(leaf('n1')))??null};
   }
   output.push({value:clone(value)});
  }catch(error){
   if(error===raw&&'error'in input)output.push({source:clone(error),identity:true});
   else if(error instanceof sdk.IvyError)output.push({ivy:error.toWire()});
   else{assert.notEqual(error.name,'ReferenceError');output.push({raw:{name:error.name,message:error.message}});}
  }
 }
 assert.equal(JSON.stringify(input),before);item.output=output;
 item.trace=input.generated?{count:trace.length,first:trace.slice(0,2),last:trace.slice(-2)}:trace;
}
const fixture={schemaVersion:1,cases};fs.writeFileSync(values.output,JSON.stringify(fixture)+'\n');
fs.writeFileSync(values.proof,JSON.stringify({at:new Date().toISOString(),node:process.version,sourceFiles:originals,releaseRoot:release,cases:cases.length,actions:cases.reduce((n,c)=>n+c.input.actions.length,0),scope:'Unchanged complete frozen TaskComments and actual CommentReader over finite objects.read, with real canonical hash and question resolver. No publication, service construction or record upgrades.'},null,2)+'\n');
console.log(JSON.stringify({cases:cases.length,actions:cases.reduce((n,c)=>n+c.input.actions.length,0)}));
