import './chat-store-loader.mjs';
const {ChatStore}=await import('../../../services/chat-bridge/src/store.ts');
const {ChatAdmission}=await import('../../../services/chat-bridge/src/admission.ts');
const {ChatOperations}=await import('../../../services/chat-bridge/src/operations.ts');
const {ChatQueries}=await import('../../../services/chat-bridge/src/queries.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {unpersistedNativePlan}=await import('../../fixtures/native-plan.ts');
const definition={workspaceId:'chat',principalId:'chat-service',rootObjectId:null,project:{serviceNodeId:'owner',namespace:'codex',kind:'project',nativeId:'/synthetic/project'},nativePlan:unpersistedNativePlan(),channels:[]};
const cases=[];
function add(label,count=1,root=null,caller='alice',edit=()=>{}){
 const def=structuredClone(definition);def.rootObjectId=root;
 def.channels=Array.from({length:count},(_,i)=>({channel:{adapter:'whatsapp',accountId:'account',channelId:'channel-'+i},displayName:'Channel '+i}));
 const admission=new ChatAdmission(def),at='2026-10-01T00:00:00.000Z';
 const value={schemaVersion:1,workspaceId:'chat',definitionHash:admission.definitionHash,identity:{channel:structuredClone(def.channels.at(-1).channel),senderPrincipalId:'another-sender',messageId:'retained'},operationId:'chat:1790000000123:original',requestHash:'sha256:'+'1'.repeat(64),binding:{objectId:'binding',revision:1},sequence:1,payload:{text:'Retained channel content',images:[]},state:'queued',nativeCalls:{resume:null,turn:null,interrupt:null},turnId:null,epoch:null,result:null,error:null,cancellation:null,admittedAt:at,updatedAt:at,finishedAt:null};
 const c={label,definition:def,caller,request:{inputId:'selected-input',expectedBridge:admission.expected(caller)},match:Math.floor((count-1)/8),page:{items:[{objectId:'selected-input',revision:2}],nextCursor:null},read:{object:{id:'selected-input',parentId:root,contractKey:'chat-bridge/input',effectivelyArchived:false},revision:{revision:2,contractVersion:'1.3.0',mediaType:'application/json'},content:{encoding:'json',value}}};
 edit(c);cases.push(c);
}
// Definition currently admits one channel, despite the historical batching comment.
for(const root of [null,'root'])for(const caller of ['alice','bob','\ud800'])add('admitted-channel-'+root+'-'+caller,1,root,caller);
add('unavailable-single-group',1,null,'alice',c=>c.match=-1);
for(const key of ['principalId','workspaceId','callerPrincipalId'])add('foreign-'+key,1,null,'alice',c=>c.request.expectedBridge[key]='other');
add('retained-expected-definition',1,null,'alice',c=>c.request.expectedBridge.definitionHash='sha256:'+'2'.repeat(64));
for(const value of [null,{},4])add('invalid-bridge-'+JSON.stringify(value),1,null,'alice',c=>c.request.expectedBridge=value);
add('missing-bridge',1,null,'alice',c=>delete c.request.expectedBridge);
add('missing-input-id',1,null,'alice',c=>delete c.request.inputId);
add('extra-query-argument',1,null,'alice',c=>c.request.extra=true);
add('invalid-input-id',1,null,'alice',c=>c.request.inputId=4);
add('null-request',1,null,'alice',c=>c.request=null);
for(const value of [true,4,'request',[]])add('primitive-request-'+JSON.stringify(value),1,null,'alice',c=>c.request=value);
for(const cursor of ['',0,false,'more',true,{},[]])add('cursor-'+JSON.stringify(cursor),1,null,'alice',c=>c.page.nextCursor=cursor);
add('missing-cursor',1,null,'alice',c=>delete c.page.nextCursor);
add('missing-items',1,null,'alice',c=>delete c.page.items);
add('null-items',1,null,'alice',c=>c.page.items=null);
add('null-first-item',1,null,'alice',c=>c.page.items=[null]);
add('original-first-item-selection',1,null,'alice',c=>c.page.items.push({objectId:'ignored',revision:7}));
for(const [label,edit]of [
 ['contract',c=>c.read.object.contractKey='other'],['version',c=>c.read.revision.contractVersion='0.0.0'],['old-readable-version',c=>c.read.revision.contractVersion='1.2.0'],
 ['media',c=>c.read.revision.mediaType='text/plain'],['encoding',c=>c.read.content.encoding='text'],['archive',c=>c.read.object.effectivelyArchived=true],
 ['parent',c=>c.read.object.parentId='foreign'],['revision',c=>c.read.revision.revision=3],['workspace',c=>c.read.content.value.workspaceId='other'],
 ['definition',c=>c.read.content.value.definitionHash='sha256:'+'3'.repeat(64)],['channel',c=>c.read.content.value.identity.channel.channelId='foreign'],
 ['identity-missing',c=>delete c.read.content.value.identity],['identity-null',c=>c.read.content.value.identity=null],
 ['channel-missing',c=>delete c.read.content.value.identity.channel],
 ['channel-before-workspace',c=>{c.read.content.value.identity.channel.channelId='foreign';c.read.content.value.workspaceId='other';}],
 ['cancelled-current-state',c=>{c.read.content.value.state='cancelled';c.read.content.value.finishedAt=c.read.content.value.updatedAt;}],
])add('read-'+label,1,null,'alice',edit);
for(const value of [null,false,0,''])add('read-falsy-channel-'+JSON.stringify(value),1,null,'alice',c=>c.read.content.value.identity.channel=value);
add('query-io-failure',1,null,'alice',c=>c.fail={method:'objects.query',query:0});
add('read-io-failure',1,null,'alice',c=>c.fail={method:'objects.read'});
for(const c of cases){
 const calls=[];let queries=0;
 const client={async request(method,params){calls.push({method,params:structuredClone(params)});const index=method==='objects.query'?queries++:null;
  if(c.fail?.method===method&&(method!=='objects.query'||c.fail.query===index))throw new IvyError('fixture_unavailable','Fixture read failed.','unknown');
  if(method==='objects.query')return structuredClone(index===c.match?c.page:{items:[],nextCursor:null});
  if(method==='objects.read')return structuredClone(c.read);throw Error('Unexpected RPC '+method);
 }};
 const store=new ChatStore(client,c.definition.rootObjectId);
 try{const value=await new ChatQueries(new ChatOperations(store,new ChatAdmission(c.definition))).input(c.caller,c.request);c.expected={value};}
 catch(error){const e=IvyError.from(error);c.expected={error:{code:e.code,message:e.message,outcome:e.outcome}};}
 finally{store.close();}
 c.calls=calls;
}
process.stdout.write(JSON.stringify(cases));
