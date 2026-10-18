import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createRequire} from 'node:module';
import {schemaSamples} from '../fixtures/schema-samples.mjs';
import {load,settings,owner,rootId,itemId,at} from './inbox-fixtures.mjs';
const {AssignmentRunner}=await load('services/secretary/src/assignment-runner.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {contractVersion}=await load('services/secretary/src/schema.js');
const {secretaryNotificationPolicy}=await load('instructions/secretary-notification-policy.js');
const {IvyError,hashJson,canonical,parseOperationId}=await load('packages/sdk/src/node.js');
const schema=JSON.parse(readFileSync('specs/schemas/secretary.schema.json','utf8'));
const {Ajv2020}=createRequire(import.meta.url)('ajv/dist/2020.js');
const ajv=new Ajv2020({strict:false,validateFormats:false,ownProperties:true});ajv.addSchema(schema);
const seed=JSON.parse(schemaSamples(ajv,[schema]).find(r=>r.valid&&r.reference.endsWith('#/$defs/Execution')).input);
const catalog=JSON.parse(readFileSync('specs/native/codex-0.159.2/catalog.json','utf8'));
const digest=c=>'sha256:'+c.repeat(64),callKey='secretary/assignment-native-call',leaseKey='secretary/thread-lease';
const target={serviceNodeId:'fixture.agent',hostId:'fixture-host',nativeVersion:catalog.version,nativeExecutableHash:catalog.nativeExecutableHash,catalogHash:hashJson(catalog)};
const baseStatus={...target,epoch:'epoch',pid:42,state:'ready',observedAt:at,code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
const decision={schemaVersion:1,urgency:'normal',notification:'main',text:'Notice',reason:'Useful'};
const rows=[];
async function add(label,c={}){
 const kind=c.kind??'run',key=kind==='start'?'threadStart':kind==='archive'?'archive':kind==='remove'?'delete':'turnStart';
 const initial={...structuredClone(seed),serviceNodeId:target.serviceNodeId,nativeTarget:target,phase:kind==='start'?'starting':kind==='archive'?'archiving':kind==='remove'?'deleting':'running',threadId:kind==='start'?null:'original-thread',turnId:c.poll?'original-turn':null,nativeOperations:{threadStart:'original-threadStart',turnStart:'original-turnStart',archive:'original-archive',delete:'original-delete'},...c.execution};
 if(kind==='start')initial.executionTarget.threadCwd='/workspace';
 if(c.fresh)initial.nativeOperations[key]=null;
 if(c.model!==undefined)initial.executionTarget.model=c.model;
 if(c.reuse)initial.assignmentSnapshot.execution={model:null,effort:null,...initial.assignmentSnapshot.execution,reuse:c.reuse};
 const current={pin:{objectId:itemId,revision:1},value:initial,writtenAt:at};
 const documents=c.poll&&c.reuse?[{id:'lease',key:leaseKey,name:c.reuse==='main'?'main':'assignment-'+initial.assignmentSnapshot.assignmentId,value:{serviceNodeId:target.serviceNodeId,threadCwd:'fixture',createdByExecutionId:'original',activeExecutionObjectId:c.competitor?'competitor':itemId,threadId:null}}]:[];
 const priorId='00000000-0000-4000-8000-000000000003';
 if(c.lease)documents.push({id:'lease',key:leaseKey,name:c.reuse==='main'?'main':'assignment-'+initial.assignmentSnapshot.assignmentId,value:{serviceNodeId:target.serviceNodeId,threadCwd:'/workspace',createdByExecutionId:'prior',activeExecutionObjectId:null,threadId:'leased-thread',...c.lease}});
 if(c.retained)documents.push({id:'call',key:callKey,name:initial.nativeOperations.threadStart,value:{operationId:initial.nativeOperations.threadStart,method:'thread/start',params:{cwd:'/retained',projectId:'retained-project',developerInstructions:'Retained original bytes'},definitionHash:digest('b'),...c.retained}});
 let revision=1,value=structuredClone(initial),store,reads=0;
 const trace=[],steps=[];const controller=new AbortController();if(c.cancel)controller.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 const read=(objectId=itemId,content=value,rev=revision)=>({object:{id:objectId,parentId:rootId,effectivelyArchived:false,contractKey:'secretary/execution'},revision:{revision:rev,contractVersion:contractVersion('secretary/execution'),mediaType:'application/json',contentHash:hashJson(content),byteLength:Buffer.byteLength(canonical(content)),references:store.objectWriteRequest('secretary/execution',content,{object:{objectId,revision:rev}},'fixture:1800000000000:read').references,createdAt:at},content:{encoding:'json',value:structuredClone(content)}});
 const client={async request(method,p,options){
  const copied=structuredClone(p);
  if(method==='objects.write'){parseOperationId(copied.mutationId);copied.mutationId='<scoped-operation>';if(c.fresh)copied.content.value.nativeOperations[key]='<generated-operation>';}
  trace.push({method,params:copied,timeout:options?.timeoutMs??35000});let result,error;
  if(c.fail===method)error={code:'fixture_failure',message:'Fixture failure.',outcome:'unknown'};
  else if(method==='serviceNodes.get')result={...owner,...c.owner};
  else if(method==='system.status')result={runtimeEpoch:'fixture'};
  else if(method==='objects.write'){assert.equal(p.expectedRevision,revision);value=structuredClone(p.content.value);revision++;result={object:{id:itemId},revision:{revision}};}
  else if(method==='objects.read'){
   if(p.objectId===priorId){
    if(c.priorError)error={code:c.priorError,message:'Fixture predecessor failure.',outcome:'not_executed'};
    else result=read(priorId,{...initial,phase:c.priorPhase??'completed',threadId:c.priorThread===undefined?'prior-thread':c.priorThread},1);
   }else result=read();
  }
  else if(method==='tools.list')result={provider:{node:{serviceNodeId:target.serviceNodeId}},items:[{qualifiedName:p.namespace+'.'+p.namePrefix,definition:{namespace:p.namespace,name:p.namePrefix,interfaceVersion:p.namespace==='agent'?'1.0.0':target.nativeVersion,...(p.namePrefix==='projects'?{annotations:{readOnlyHint:true}}:{}),inputSchema:{},outputSchema:{}},definitionHash:digest(p.namespace==='agent'?'a':'b'),catalogHash:target.catalogHash}],nextCursor:null};
  else if(method==='tools.call'){
   if(c.failTool===p.qualifiedName)error={code:'fixture_failure',message:'Fixture failure.',outcome:'unknown'};
   else if(p.qualifiedName==='agent.status')result={...baseStatus,...c.status};
   else if(p.qualifiedName==='agent.projects')result={source:'native',observedAt:at,projects:[{nativeId:'project',name:'project',paths:['/workspace'],source:'native'}],defaults:{projectRoot:'/projects',internalProjectRoot:'/internal'}};
   else if(p.qualifiedName==='agent.operation'){
    const call=store.technicalNamed(callKey,p.arguments.operationId).value;
    result={schemaVersion:1,operationId:call.operationId,callerPrincipalId:settings.identity.principalId,serviceNodeId:target.serviceNodeId,nativeVersion:target.nativeVersion,nativeExecutableHash:target.nativeExecutableHash,method:call.method,params:call.params,requestHash:hashJson({method:call.method,params:call.params}),createdAt:at,updatedAt:at,phase:'succeeded',epoch:'epoch',requestId:42,reply:{result:kind==='run'?(c.reply??{turn:{id:'new-turn'}}):kind==='start'?(c.reply??{thread:{id:'new-thread'}}):null},code:null,...c.operation};
   }else if(p.qualifiedName==='agent.read'){
    const {method,params}=p.arguments;const full=params.itemsView==='full';
    const items=c.items??[{id:'final',type:'agentMessage',phase:'final_answer',text:JSON.stringify(decision)}];
    const turn={id:'original-turn',status:full?(c.fullStatus??c.turnStatus??'completed'):(c.turnStatus??'completed'),itemsView:params.itemsView,items:full?items:[],...c.row};
    const reply=method==='thread/items/list'?{error:{code:c.itemError??-32601,message:'Fixture full-turn fallback'}}:c.readReply??{result:{data:[turn]}};
    result={schemaVersion:1,observationId:'read-'+(++reads),callerPrincipalId:settings.identity.principalId,serviceNodeId:target.serviceNodeId,nativeVersion:target.nativeVersion,nativeExecutableHash:target.nativeExecutableHash,catalogHash:target.catalogHash,epoch:'epoch',requestId:42,observedAt:at,requestHash:hashJson({method,params}),method,params,reply,...c.observation};
   }else assert.fail('Unexpected tool '+p.qualifiedName);
  }else assert.fail('Unexpected RPC '+method);
  if(error){steps.push({method,error});throw new IvyError(error.code,error.message,error.outcome);}
  steps.push({method,value:structuredClone(result)});return result;
 }};
 store=new SecretaryStore(client,rootId);
 for(const d of documents){const encoded=canonical(d.value);store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(d.id,d.key,d.name,1,encoded);store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(d.id,1,encoded);}
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings,owner:{serviceNodeId:'fixture.secretary'},signal:controller.signal,now:()=>new Date(c.time??at)});
 const runner=new AssignmentRunner(engine);let result;
 try{result={value:await runner[kind](current)};}catch(cause){const e=IvyError.from(cause);result={code:e.code,message:e.message,outcome:e.outcome};}
 // Random technical IDs are irrelevant; retained names, revisions and original bytes are not.
 const remaining=store.db.prepare('SELECT key,name,current_revision,value_json FROM technical_documents ORDER BY key,name').all().map(r=>({key:r.key,name:r.name,revision:r.current_revision,value:JSON.parse(r.value_json)}));
 store.close();rows.push({input:{label,kind,key,current,documents,steps,cancel:c.cancel??false,fresh:c.fresh??false,time:Date.parse(c.time??at)},expected:{result,trace,remaining}});
}
for(const kind of ['run','archive','remove']){
 await add(kind,{kind});await add(kind+'-fresh',{kind,fresh:true});
 for(const threadId of [null,''])await add(kind+'-missing-thread-'+threadId,{kind,execution:{threadId}});
 await add(kind+'-cancelled',{kind,cancel:true});
 await add(kind+'-owner-changed',{kind,owner:{principalId:'other'}});
 for(const fail of ['objects.write','objects.read'])await add(kind+'-'+fail,{kind,fail});
 await add(kind+'-pending',{kind,operation:{phase:'accepted',epoch:null,requestId:null,reply:null}});
 await add(kind+'-failed',{kind,operation:{phase:'failed',reply:{error:{code:1,message:'Failed'}},code:'native_failed'}});
 await add(kind+'-no-target',{kind,execution:{nativeTarget:null}});
}
for(const model of [null,'','original-model'])await add('model-'+model,{model});
for(const reply of [{},{turn:{}},{turn:{id:null}},{turn:{id:0}},{turn:{id:'\ud800'}}])await add('reply-'+rows.length,{reply});
for(const time of ['2000-02-26T23:59:59.999Z','1969-12-31T23:59:59.999Z','9999-12-31T00:00:00.000Z'])await add('retention-'+time,{kind:'archive',time});
for(const turnStatus of ['inProgress','completed','failed','interrupted'])await add('poll-'+turnStatus,{poll:true,turnStatus});
for(const reuse of ['main','assignment'])for(const competitor of [false,true])await add('lease-'+reuse+'-'+competitor,{poll:true,reuse,competitor});
await add('poll-owner-changed',{poll:true,owner:{connected:false}});
await add('poll-save-failed',{poll:true,fail:'objects.write',reuse:'main'});
await add('poll-wrong-turn',{poll:true,row:{id:'other'}});
await add('poll-no-turns',{poll:true,readReply:{result:{data:[]}}});
await add('poll-epoch-mismatch',{poll:true,observation:{epoch:'other'}});
await add('poll-item-read-error',{poll:true,itemError:1});
await add('poll-status-changed',{poll:true,fullStatus:'failed'});
await add('poll-duplicate-items',{poll:true,items:[{id:'same'},{id:'same'}]});
await add('poll-invalid-answer',{poll:true,items:[{id:'final',type:'agentMessage',phase:'final_answer',text:'not-json'}]});
await add('poll-not-ready',{poll:true,status:{epoch:null,state:'stopped',pid:null}});
const start=c=>({kind:'start',...c}),priorId='00000000-0000-4000-8000-000000000003';
await add('start-new',start({}));await add('start-fresh-operation',start({fresh:true}));
await add('start-retained-original',start({retained:{}}));
await add('start-retained-method-mismatch',start({retained:{method:'thread/resume'}}));
for(const reuse of ['new','main','assignment'])await add('start-reuse-'+reuse,start({reuse}));
for(const model of [null,'original-model'])await add('start-model-'+model,start({model}));
for(const status of [{},{epoch:null},{epoch:''},{serviceNodeId:'other'},{nativeVersion:null}])await add('target-'+rows.length,start({execution:{nativeTarget:null},status}));
await add('target-keeps-start-time',start({execution:{nativeTarget:null,startedAt:'2020-01-01T00:00:00.000Z'}}));
for(const fail of ['objects.write','objects.read'])await add('target-'+fail,start({execution:{nativeTarget:null},fail}));
for(const lease of [{},{activeExecutionObjectId:itemId},{threadId:null},{threadId:'',createdByExecutionId:seed.executionId},{threadId:null,createdByExecutionId:seed.executionId},{serviceNodeId:'other'},{threadCwd:'/other'}])await add('start-lease-'+rows.length,start({reuse:'main',lease}));
for(const priorPhase of ['queued','preflight','starting','running','archiving','deleting','completed','archived','settled','deleted','failed'])await add('start-prior-'+priorPhase,start({reuse:'assignment',lease:{activeExecutionObjectId:priorId,threadId:null},priorPhase}));
for(const priorError of ['not_found','fixture_failure'])await add('start-prior-'+priorError,start({reuse:'main',lease:{activeExecutionObjectId:priorId,threadId:null},priorError}));
await add('start-prior-preserves-lease-thread',start({reuse:'main',lease:{activeExecutionObjectId:priorId},priorPhase:'completed'}));
await add('start-existing-execution-thread',start({reuse:'main',execution:{threadId:'original-thread'},lease:{createdByExecutionId:seed.executionId}}));
for(const fail of ['objects.write','objects.read'])await add('start-'+fail,start({reuse:'main',fail}));
for(const failTool of ['agent.projects','agent.operation'])await add('start-'+failTool,start({reuse:'main',failTool}));
await add('start-cancelled',start({reuse:'main',cancel:true}));
await add('start-owner-changed',start({reuse:'main',owner:{principalId:'other'}}));
await add('start-pending',start({reuse:'main',operation:{phase:'accepted',epoch:null,requestId:null,reply:null}}));
for(const reply of [{},{thread:{id:''}},{thread:{id:0}},{thread:{id:'\ud800'}}])await add('start-reply-'+rows.length,start({reply}));
process.stdout.write(JSON.stringify({settings,rootId,notificationPolicy:secretaryNotificationPolicy,rows}));
