import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const root=resolve(process.env.IVY_SECRETARY_ORACLE??'.local/build/secretary');
const load=p=>import(pathToFileURL(resolve(root,p)));
const engineModule=await load('services/secretary/src/engine.js');
const {SecretaryEngine,operationName,sourceName,identityHash,messageHash,noticeText}=engineModule;
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {IvyError,canonical,hashJson}=await load('packages/sdk/src/node.js');
const {validate}=await load('services/secretary/src/schema.js');
const rootId='00000000-0000-4000-8000-000000000001',itemId='00000000-0000-4000-8000-000000000002',sourceId='00000000-0000-4000-8000-000000000003',operationId='00000000-0000-4000-8000-000000000004';
const at='2026-01-01T00:00:00.000Z',id='fixture-epoch:1800000000000:inbox',caller='producer';
const scope={secretaryId:'secretary',rootObjectId:rootId};
const definition={sourceId:'mail',accountId:'account',kind:'email',producerPrincipalIds:[caller],allowedSenderIds:null,ownSenderIds:['self']};
const source={schemaVersion:1,definition,definitionHash:hashJson(definition),cursor:null,checkpointOperation:null,updatedAt:null};
const message={accountId:'account',conversationId:'conversation',messageId:'message',nativeRevision:'revision',senderId:'sender',outgoing:false,attachments:'none',occurredAt:at,observedAt:at,title:'Subject',text:'Full text',url:null};
const policy={silentTime:null,urgentBypass:false,whatsappQuestionsOnly:false,minimumMainUrgency:'normal',researchMaxMinutes:0,voiceEscalation:null};
const settings={identity:{scope,principalId:'secretary-principal',hostId:'fixture-host'},sources:[definition],policy,pollMs:100,recordsPerTick:10};
const owner={connected:true,synced:true,principalId:'secretary-principal',hostId:'fixture-host',serviceName:'secretary'};
const assessment={urgency:'normal',disposition:'notify',notification:'main',summary:'Summary',question:false,usefulIntel:true,task:null};
const item={schemaVersion:1,sourceId:'mail',identityHash:identityHash('mail',message),messageHash:messageHash(message),message,media:[],decision:null,notice:null};
const request={action:'capture',operationId:id,expectedScope:scope,sourceId:'mail',message};
const accepted={schemaVersion:1,action:'capture',operationId:id,callerPrincipalId:caller,requestHash:hashJson(request),request,sourceId:'mail',source,createdAt:at,prepared:null,existingEffect:null,ignoredReason:null,captureIdentity:item.identityHash,checkpointCaptures:[],phase:'accepted',effect:null,errorCode:null};
for(const [name,value] of Object.entries({Source:source,Message:message,Item:item,Accepted:accepted}))validate(name,value);
const sourceLocal=value=>({key:'secretary/source',id:sourceId,name:sourceName('mail'),value});
const operationLocal=(value,first=value)=>({key:'secretary/operation',id:operationId,name:operationName(caller,id),value,first});
const document=(value,revision=1)=>({pin:{objectId:operationId,revision},value});
const readItem=value=>({object:{id:itemId,parentId:rootId,effectivelyArchived:false,contractKey:'secretary/item'},revision:{revision:2,contractVersion:'1.3.0',mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:{},createdAt:at},content:{encoding:'json',value}});
const step=(method,value)=>({method,value});
const rows=[];
async function add(method,args,{locals=[sourceLocal(source)],steps=[],config=settings,cancelled=false}={}){
 const input=structuredClone({method,args,locals,steps,settings:config,cancelled}),pending=structuredClone(steps),trace=[];
 const client={async request(method,params,options){trace.push({method,params:structuredClone(params),timeout:options?.timeoutMs??35000});const next=pending.shift();assert.ok(next,`Unexpected ${method}`);assert.equal(next.method,method);if(next.error)throw new IvyError(next.error.code,next.error.message,next.error.outcome);return structuredClone(next.value);}};
 const store=new SecretaryStore(client,rootId),abort=new AbortController();if(cancelled)abort.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 for(const local of locals){const first=local.first??local.value,revision=hashJson(first)===hashJson(local.value)?1:2,encoded=canonical(local.value);store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(local.id,local.key,local.name,revision,0,null,encoded);store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(local.id,1,canonical(first));if(revision===2)store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(local.id,2,encoded);}
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings:structuredClone(config),owner:{serviceNodeId:'fixture.secretary',generation:1},signal:abort.signal});
 let result;
 try{const value=await (engineModule[method]?engineModule[method](...structuredClone(args)):engine[method](...structuredClone(args)));result={value:value??null};}
 catch(cause){const e=IvyError.from(cause);result={code:e.code,message:e.message,outcome:e.outcome};}finally{store.close();}
 rows.push({input,expected:{result,trace,remaining:pending.length}});
}
for(const text of ['','a','é😀','\ud800','a\0b']){
 await add('operationName',[text,id]);await add('sourceName',[text]);
 await add('identityHash',[text,{...message,senderId:text}]);await add('messageHash',[{...message,text,observedAt:'changed'}]);
}
for(const kind of ['email','teams','whatsapp'])for(const senderId of ['sender','user:123','application:456','User:123',''])for(const title of ['','Subject'])await add('noticeText',[assessment,{...message,senderId,title},{...definition,kind}]);
for(const text of ['', '😀'.repeat(8192),'a'.repeat(8179)+'😀','\ud800'])await add('noticeText',[{...assessment,summary:text},{...message,text},{...definition,kind:'teams'}]);
await add('sourceIdentity',[definition]);
for(const action of ['capture','checkpoint','attach','assess','operation','status'])for(const principal of [caller,'other'])await add('authorize',[principal,scope,action,'mail']);
await add('authorize',[caller,{...scope,rootObjectId:'other'},'capture','mail']);await add('authorize',[caller,scope,'capture']);await add('authorize',[caller,scope,'capture','unknown']);
await add('sourceDefinition',['mail']);await add('sourceDefinition',['missing']);
await add('verifyOwner',[],{steps:[step('serviceNodes.get',owner)]});await add('verifyOwner',[],{cancelled:true});
for(const [key,value] of [['connected',false],['synced',false],['principalId','other'],['hostId','other'],['serviceName','other']])await add('verifyOwner',[],{steps:[step('serviceNodes.get',{...owner,[key]:value})]});
await add('source',['mail']);await add('source',['missing']);
await add('source',['mail'],{config:{...settings,sources:[]}});
for(const [key,value] of [['kind','teams'],['accountId','other'],['producerPrincipalIds',['other']]])await add('source',['mail'],{config:{...settings,sources:[{...definition,[key]:value}]}});
await add('source',['mail'],{locals:[sourceLocal({...source,definitionHash:'sha256:'+'0'.repeat(64)})]});
for(const value of [item,{...item,identityHash:'sha256:'+'0'.repeat(64)},{...item,messageHash:'sha256:'+'0'.repeat(64)},{...item,message:{...message,observedAt:'2026-01-02T00:00:00.000Z'}},{...item,message:{...message,text:'tampered'}},{...item,message:{...message,accountId:'other'}}])await add('item',[itemId],{steps:[step('objects.read',readItem(value))]});
const assessed={...item,decision:{assessment,actor:caller,operationId:id,decidedAt:at,policy},notice:{state:'queued',reason:null,text:noticeText(assessment,message,definition),source:{objectId:itemId,revision:2},target:null,request:null,preparedRevision:null,evidence:null,voice:null,updatedAt:at}};
for(const value of [assessed,{...assessed,notice:null},{...assessed,decision:null},{...assessed,notice:{...assessed.notice,text:'changed'}},{...assessed,notice:{...assessed.notice,source:{objectId:sourceId,revision:2}}}])await add('item',[{objectId:itemId,revision:2}],{steps:[step('objects.read',readItem(value))]});
await add('checked',[document(accepted)]);
for(const change of [{request:null},{requestHash:'sha256:'+'0'.repeat(64)},{action:'assess'},{operationId:'other'},{request:{...request,expectedScope:{...scope,secretaryId:'other'}}},{source:{...source,definitionHash:'sha256:'+'0'.repeat(64)}},{source:{...source,definition:{...definition,accountId:'other'},definitionHash:hashJson({...definition,accountId:'other'})}}])await add('checked',[document({...accepted,...change})]);
await add('checked',[document({...accepted,request:null,ignoredReason:'outgoing'})]);
const completed={...accepted,phase:'succeeded',effect:{objectId:itemId,revision:2}};
await add('checked',[document(completed,2)],{locals:[sourceLocal(source),operationLocal(completed,accepted)]});
for(const first of [{...accepted,phase:'succeeded'},{...accepted,effect:{objectId:itemId,revision:2}},{...accepted,errorCode:'error'},{...accepted,requestHash:'sha256:'+'0'.repeat(64)}])await add('checked',[document(completed,2)],{locals:[sourceLocal(source),operationLocal(completed,first)]});
await add('find',[caller,id]);await add('find',[caller,id],{locals:[sourceLocal(source),operationLocal(accepted)]});
for(const change of [{callerPrincipalId:'other'},{operationId:'other'}])await add('find',[caller,id],{locals:[sourceLocal(source),operationLocal({...accepted,...change})]});
await add('operation',[caller,scope,id],{steps:[step('serviceNodes.get',owner)],locals:[sourceLocal(source),operationLocal(completed,accepted)]});
await add('operation',[caller,scope,id],{steps:[step('serviceNodes.get',owner)]});
await add('operation',[caller,{...scope,secretaryId:'other'},id]);
process.stdout.write(JSON.stringify({root:rootId,rows}));
