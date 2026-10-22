import assert from 'node:assert/strict';
import {resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {load,settings as base,owner as secretaryOwner,rootId} from './inbox-fixtures.mjs';
const {TeamsNative,teamsName}=await load('services/secretary/src/teams-native.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {SecretaryEngine}=await load('services/secretary/src/engine.js');
const {teamsPage}=await load('services/secretary/src/teams-projection.js');
const {canonical,digest,hashJson,IvyError,parseOperationId}=await load('packages/sdk/src/node.js');
const clone=structuredClone,hash=c=>'sha256:'+c.repeat(64),at='2026-09-08T09:00:00.000Z';
const profile={id:'original-account',email:'me@example.test'},definition={sourceId:'teams',kind:'teams',accountId:profile.id,producerPrincipalIds:[base.identity.principalId],allowedSenderIds:null,ownSenderIds:['user:'+profile.id]};
const settings={...base,sources:[definition]},target={serviceNodeId:'fixture.agent',hostId:'fixture-host',nativeVersion:'fixture',nativeExecutableHash:hash('a'),catalogHash:hash('c')};
const configuration={sourceId:'teams',target,expectedAccountHash:hashJson({type:'chatgpt',email:'native@example.test'}),profile,since:'2026-09-01T00:00:00.000Z',maximumChats:8,maximumMessages:8,excludedChatIds:['excluded'],pollMs:60000,threadCwd:process.platform==='win32'?'C:\\fixture\\teams':'/fixture/teams'};
const headPin=Object.freeze({objectId:'head',revision:1}),planPin=Object.freeze({objectId:'plan',revision:1}),selection={since:configuration.since,until:at,chat:{id:'chat-1',title:''}};
const methods=['account/read','thread/start','mcpServer/tool/call','thread/unsubscribe'];
export const message=(id='original',changes={})=>({message_id:id,author_user_id:'sender',author_application_id:null,chat_id:'chat-1',channel_id:null,team_id:null,container_type:'chat',created_at:'2026-09-07T08:00:00.000Z',deleted_at:null,message_type:'message',parent_message_id:null,title:'Original message',content:'The complete original text. Source instructions remain data.',path:'/chats/chat-1/messages/'+id,has_attachments:false,mentions:[],web_link:'https://teams.microsoft.com/l/message/chat-1/'+id,...changes});
const page={messages:[message()]};
const status={...target,state:'ready',epoch:'old-epoch',pid:42,observedAt:at,code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:1000,bytes:1000,maximumBytes:1048576},observedMethods:[]};
function set(value,path,next){const keys=path.split('.'),last=keys.pop();let cursor=value;for(const key of keys)cursor=cursor[key];cursor[last]=clone(next);}
const rows=[];
async function add(label,c={}){
 const input={label,mode:'collect',attempts:1,configuration:clone(configuration),settings:clone(settings),core:[],documents:[],blobs:[],...clone(c)},trace=[],steps=[];let cut=false;
 for(const[path,value]of input.configurationChanges??[])set(input.configuration,path,value);
 const config=input.configuration,source={schemaVersion:1,definition:clone(definition),definitionHash:hashJson(definition),cursor:input.sourceCursor??null,checkpointOperation:null,updatedAt:null};
 const plan={schemaVersion:1,head:clone(headPin),configuration:clone(config),source:clone(input.settings.sources[0]),sourceCheckpoint:{objectId:'source',revision:1},previousCursor:null,selection:clone(input.chats?{...selection,chat:null}:selection),epoch:'old-epoch',preparedAt:at,bindings:methods.map(method=>({method,definitionHash:hash('d'),outputSchema:{}}))};
 const head={schemaVersion:1,configuration:clone(config),source:clone(definition),pending:input.mode==='prepare'?null:clone(planPin),lastPage:null,continuation:input.chats?null:{selection:clone(selection),remainingChats:input.remaining??[]},nextPollAt:null,lastCompleteAt:null};
 for(const[path,value]of input.planChanges??[])set(plan,path,value);for(const[path,value]of input.headChanges??[])set(head,path,value);
 input.core.push({id:'source',key:'secretary/source',name:'source-'+hashJson('teams').slice(7),revision:1,value:source});
 input.documents.push({id:headPin.objectId,key:'secretary/teams-head',name:teamsName('head','teams'),revision:input.mode==='prepare'?1:2,value:head});
 if(!input.noPlan)input.documents.push({id:planPin.objectId,key:'secretary/teams-plan',name:teamsName('plan',headPin),revision:1,value:plan});
 const operations=new Map();
 function invocation(slot){const threadId='original-thread';
  if(slot===0||slot===5)return{method:'account/read',params:{refreshToken:false}};
  if(slot===1)return{method:'thread/start',params:{cwd:config.threadCwd,ephemeral:true,permissions:':danger-full-access',approvalPolicy:'never'}};
  if(slot===6)return{method:'thread/unsubscribe',params:{threadId}};
  return{method:'mcpServer/tool/call',params:{threadId,server:'codex_apps',tool:'microsoft_teams.'+(slot===3?plan.selection.chat?'list_chat_messages':'list_chats':'get_profile'),arguments:slot===3?plan.selection.chat?{chat_id:plan.selection.chat.id,sent_after:config.since,top:config.maximumMessages+1}:{unread_only:true,top:config.maximumChats+1}:{}}};
 }
 function operation(slot,phase='succeeded'){
  const call=invocation(slot);let result=slot===0||slot===5?{account:{type:'chatgpt',email:input.accountMismatch===slot?'foreign@example.test':'native@example.test'},requiresOpenaiAuth:true}:slot===1?{thread:{id:'original-thread'}}:slot===6?{status:input.cleanupStatus??'unsubscribed'}:{content:[],isError:false,structuredContent:slot===3?(input.page??(input.chats?{chats:[{id:'chat-1',topic:null,is_unread:true,is_hidden:false}]}:page)):input.profileMismatch===slot?{...profile,email:'other@example.test'}:profile};
  if(input.rateLimited&&slot===3)result={content:[],isError:true,structuredContent:{error_code:'RATE_LIMITED',retry_after_seconds:62}};
  const value={schemaVersion:1,operationId:'fixture-epoch:1800000000000:teams-slot-'+slot,callerPrincipalId:settings.identity.principalId,serviceNodeId:target.serviceNodeId,nativeVersion:target.nativeVersion,nativeExecutableHash:target.nativeExecutableHash,...call,requestHash:hashJson(call),phase,createdAt:at,updatedAt:at,epoch:phase==='accepted'?null:slot===input.wrongEpochSlot?'other-epoch':'old-epoch',requestId:phase==='accepted'?null:42+slot,reply:phase==='succeeded'?{result:clone(result)}:phase==='failed'?{error:{code:-1,message:'Native failed'}}:null,code:phase==='failed'?'native_failed':phase==='outcome_unknown'?'outcome_unknown':null};
  if(input.operationChanges?.slot===slot)for(const[path,v]of input.operationChanges.changes)set(value,path,v);return value;
 }
 function blob(slot,kind,value){const bytes=Buffer.from(canonical(value)),contentHash=digest(bytes),id=kind+'-'+slot;input.blobs.push({id,key:'secretary/microsoft-native',name:'microsoft-operation-'+hashJson([planPin,slot,contentHash]).slice(7),bytes:bytes.toString('base64')});return{byteLength:bytes.length,contentHash,chunks:[{objectId:id,revision:1}]};}
 if(!input.noCalls&&input.mode!=='prepare')for(let slot=0;slot<7;slot++){
  const call={schemaVersion:1,plan:clone(planPin),slot,...invocation(slot),operationId:'fixture-epoch:1800000000000:teams-slot-'+slot,definitionHash:hash('d'),preparedAt:at,seen:null,observation:null};
  const op=operation(slot,input.failedSlot===slot?'failed':input.pendingSlot===slot?'accepted':'succeeded');operations.set(call.operationId,op);
  if(input.allRetained)call.observation=blob(slot,'observation',op);
  if(input.seenSlot===slot){const previous=operation(slot,input.seenPhase??'dispatched');if(input.regress)previous.updatedAt='2026-09-08T09:00:00.002Z';call.seen=blob(slot,'seen',previous);}
  if(input.callChanges?.slot===slot)for(const[path,value]of input.callChanges.changes)set(call,path,value);
  input.documents.push({id:'call-'+slot,key:'secretary/teams-call',name:teamsName('call',[planPin,slot]),revision:1,value:call});
 }
 if(input.mode==='evidence'){
  const proof={schemaVersion:1,plan:clone(planPin),calls:Array.from({length:7},(_,i)=>({objectId:'call-'+i,revision:1})),observedAt:at,gap:null,retryAfterSeconds:null,page:teamsPage(input.page??(input.chats?{chats:[]}:page),config,definition,plan.selection,at)};
  for(const[path,value]of input.proofChanges??[])set(proof,path,value);
  input.documents.push({id:'proof',key:'secretary/teams-evidence',name:teamsName('evidence',planPin),revision:1,value:proof});
 }
 let store;
 const binding=(namespace,name)=>({qualifiedName:namespace+'.'+name,definition:{namespace,name,interfaceVersion:namespace==='agent'?'1.0.0':target.nativeVersion,inputSchema:{},outputSchema:{}},definitionHash:hash(namespace==='agent'?'e':'d'),catalogHash:target.catalogHash});
 const client={async request(method,p,options){
  trace.push({method,params:clone(p),timeout:options?.timeoutMs??35000});let value;
  try{
   if(method==='serviceNodes.get')value={...secretaryOwner,...input.owner};
   else if(method==='system.status')value={runtimeEpoch:'fixture-epoch'};
   else if(method==='tools.list')value={provider:{node:{serviceNodeId:target.serviceNodeId}},items:[binding(p.namespace,p.namePrefix)],nextCursor:null};
   else if(method==='tools.call'){
    if(p.qualifiedName==='agent.status')value={...status,epoch:input.currentEpoch??status.epoch,...input.status};
    else if(p.qualifiedName==='agent.operation'){
     if(input.noCalls)throw new IvyError('fixture_cut','Stopped after durable original call preparation.','unknown');
     const id=p.arguments.operationId,slot=Number(id.slice(id.lastIndexOf('-')+1));
     if(input.absentSlot===slot&&!input.invoked?.includes(slot))throw new IvyError('not_found','Original operation absent.','not_executed',{kind:'agent_operation_absent',serviceNodeId:target.serviceNodeId,operationId:id,epoch:input.currentEpoch??'old-epoch'});
     value=operations.get(id);assert(value,'Original native operation required');
    }else if(p.qualifiedName==='agent.invoke'){
     const original=p.arguments,slot=Number(original.operationId.slice(original.operationId.lastIndexOf('-')+1));assert.deepEqual(original.params,invocation(slot).params);assert.equal(original.expectedDefinitionHash,hash('d'));
     input.invoked??=[];input.invoked.push(slot);value=operations.get(original.operationId);if(input.invokeUnknown)throw new IvyError('fixture_send_failed','The exact original native send is uncertain.','unknown');
    }else assert.fail('Unexpected tool '+p.qualifiedName);
   }else assert.fail('Unexpected RPC '+method);
   if(input.cutAfter===trace.length&&!cut){cut=true;throw new IvyError('fixture_cut','One original read reply was lost.','unknown');}
   steps.push({method,value:clone(value)});return clone(value);
  }catch(e){const x=IvyError.from(e);steps.push({method,error:{code:x.code,message:x.message,outcome:x.outcome,...(x.details===undefined?{}:{details:x.details})}});throw e;}
 }};
 store=new SecretaryStore(client,rootId);
 for(const row of input.core){const encoded=canonical(row.value);store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(row.id,row.key,row.name,row.revision,0,null,encoded);store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(row.id,row.revision,encoded);}
 for(const row of input.documents){const encoded=canonical(row.value);store.db.prepare('INSERT INTO technical_documents VALUES(?,?,?,?,?)').run(row.id,row.key,row.name,row.revision,encoded);store.db.prepare('INSERT INTO technical_revisions VALUES(?,?,?)').run(row.id,row.revision,encoded);}
 for(const row of input.blobs)store.db.prepare('INSERT INTO technical_blobs VALUES(?,?,?,?)').run(row.id,row.key,row.name,Buffer.from(row.bytes,'base64'));
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings:input.settings,epoch:Promise.resolve('fixture-epoch'),owner:{serviceNodeId:'fixture.secretary'},signal:new AbortController().signal,now:()=>new Date(at)}),results=[];
 for(let attempt=0;attempt<input.attempts;attempt++)try{
  const native=new TeamsNative(engine,config);let value;
  if(input.mode==='constructor')value=null;
  else if(input.mode==='head')value=await native.rawHead();
  else if(input.mode==='plan')value=await native.plan(planPin);
  else if(input.mode==='prepare')value=await native.prepare(await native.rawHead());
  else if(input.mode==='evidence')value=await native.evidence({objectId:'proof',revision:1});
  else{
   value=await native.collect(await native.plan(planPin));
   if(value&&input.nextHead)value={proof:value,nextHead:native.nextHead(head,await native.plan(planPin),value)};
  }
  results.push({value:value??null});
 }catch(e){const error=IvyError.from(e);results.push({code:error.code,message:error.message,outcome:error.outcome,...(error.details===undefined?{}:{details:error.details})});}
 const docs=store.db.prepare('SELECT id,key,name,current_revision,value_json FROM technical_documents ORDER BY key,name').all(),blobs=store.db.prepare('SELECT * FROM technical_blobs ORDER BY key,name').all();
 const ids=new Map([...docs.map(r=>[r.id,'document:'+r.key+':'+r.name]),...blobs.map(r=>[r.id,'blob:'+r.key+':'+r.name])]);
 if(input.noCalls)for(const row of docs.filter(r=>r.key==='secretary/teams-call')){const call=JSON.parse(row.value_json);parseOperationId(call.operationId);ids.set(call.operationId,'fresh-native-slot-'+call.slot);}
 const normalize=v=>typeof v==='string'?(ids.get(v)??v):Array.isArray(v)?v.map(normalize):v&&typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,c])=>[k,normalize(c)])):v;
 const retained={documents:docs.map(r=>({id:r.id,key:r.key,name:r.name,revision:r.current_revision,value:JSON.parse(r.value_json)})),revisions:store.db.prepare('SELECT id,revision,value_json FROM technical_revisions').all().map(r=>({id:r.id,revision:r.revision,value:JSON.parse(r.value_json)})),blobs:blobs.map(r=>({id:r.id,key:r.key,name:r.name,bytes:Buffer.from(r.bytes).toString('base64')}))};retained.revisions.sort((a,b)=>normalize(a.id).localeCompare(normalize(b.id),'en')||a.revision-b.revision);
 input.steps=steps;delete input.invoked;store.close();rows.push({input,expected:normalize({results,trace,retained})});
}
await add('complete retained original read',{nextHead:true});await add('complete original read replay',{attempts:2,nextHead:true});await add('all retained receipts need no native polling',{allRetained:true,nextHead:true});
await add('chat list selects the first bounded conversation',{chats:true,nextHead:true});
await add('remaining chats survive original page completion',{remaining:[{id:'chat-2',title:'Second'},{id:'chat-3',title:'Third'}],nextHead:true});
await add('exposed continuation becomes a gap',{page:{...page,has_more:true},nextHead:true});
await add('maximum message overflow becomes a gap',{page:{messages:Array.from({length:9},(_,i)=>message(String(i)))},nextHead:true});
await add('Teams sender configuration is required',{settings:{...settings,sources:[{...definition,ownSenderIds:['me@example.test']}]},mode:'constructor'});
await add('original pending native call',{pendingSlot:3});await add('retained pending progress',{seenSlot:3});await add('retained unknown progress',{seenSlot:3,seenPhase:'outcome_unknown'});await add('terminal cannot be retained as seen',{seenSlot:3,seenPhase:'succeeded'});await add('original progress cannot regress',{seenSlot:0,regress:true});
for(const slot of [0,5])await add('account gate '+slot,{accountMismatch:slot,nextHead:true});for(const slot of [2,4])await add('profile gate '+slot,{profileMismatch:slot,nextHead:true});
for(const slot of [0,1,3,5,6])await add('original failed native '+slot,{failedSlot:slot,nextHead:true});
await add('cleanup reply must prove release',{cleanupStatus:'stillLoaded'});await add('idempotent cleanup status',{cleanupStatus:'notSubscribed',nextHead:true});await add('rate limit retains retry delay',{rateLimited:true,nextHead:true});
await add('read epoch mismatch',{wrongEpochSlot:3,nextHead:true});await add('confirmed original absence allows guarded dispatch',{absentSlot:3});await add('uncertain dispatch reads original receipt',{absentSlot:3,invokeUnknown:true});await add('seen work cannot become absent',{absentSlot:3,seenSlot:3});
await add('new epoch records original boundary before later completion',{absentSlot:0,currentEpoch:'new-epoch',attempts:2,nextHead:true});await add('new epoch cleans original thread',{absentSlot:3,currentEpoch:'new-epoch',attempts:2,nextHead:true});
await add('changed source cursor refuses original dispatch',{absentSlot:0,sourceCursor:'advanced'});await add('changed head refuses original dispatch',{absentSlot:0,headChanges:[['pending',{objectId:'other',revision:1}]]});await add('owner refusal before persistence',{owner:{principalId:'other'}});
await add('fresh status epoch must still match absence',{absentSlot:0,status:{epoch:'new-epoch'}});await add('stale boundary observation cannot complete original plan',{absentSlot:0,currentEpoch:'new-epoch',status:{observedAt:'2026-09-08T08:59:59.999Z'},attempts:2});
await add('fresh call is durable before owner read',{noCalls:true});
for(const cutAfter of [1,3,7,12,20])await add('original reply loss '+cutAfter,{cutAfter,attempts:2});
for(const[path,value]of [['profile.id','other'],['threadCwd','relative'],['since','2026-02-30T00:00:00.000Z']])await add('constructor '+path,{mode:'constructor',configurationChanges:[[path,value]]});
for(const[path,value]of [['configuration.expectedAccountHash',hash('f')],['bindings.0.method','thread/start'],['preparedAt','2026-02-30T00:00:00.000Z'],['selection.until',selection.since],['selection.since','2026-09-02T00:00:00.000Z']])await add('retained plan '+path,{mode:'plan',planChanges:[[path,value]]});
for(const[path,value]of [['params.refreshToken',true],['definitionHash',hash('f')],['preparedAt','2026-09-08T08:59:59.999Z'],['plan.objectId','other']])await add('retained call '+path,{callChanges:{slot:0,changes:[[path,value]]}});
for(const[path,value]of [['callerPrincipalId','other'],['operationId','other'],['requestHash',hash('f')]])await add('native receipt '+path,{operationChanges:{slot:0,changes:[[path,value]]}});
await add('original evidence reconstructs all calls',{mode:'evidence',allRetained:true});await add('incompatible saved evidence',{mode:'evidence',allRetained:true,proofChanges:[['observedAt','2026-09-08T09:00:00.001Z']]});
await add('existing plan prepare only claims matching head',{mode:'prepare'});await add('fresh plan retains exact discovered bindings',{mode:'prepare',noPlan:true});await add('deferred poll remains idle',{mode:'prepare',headChanges:[['nextPollAt','2026-09-08T09:01:00.000Z']]});
for(const label of ['complete retained original read','all retained receipts need no native polling','confirmed original absence allows guarded dispatch','uncertain dispatch reads original receipt','original evidence reconstructs all calls','fresh plan retains exact discovered bindings'])assert(rows.find(r=>r.input.label===label).expected.results.every(v=>'value'in v),label);
export const teamsNativeCorpus={rootId,nodeId:'fixture.secretary',time:Date.parse(at),wall:1800000000000,rows};
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url))console.log(JSON.stringify(teamsNativeCorpus));
