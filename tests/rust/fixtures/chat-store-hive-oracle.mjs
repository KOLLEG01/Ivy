import './chat-store-loader.mjs';
import assert from 'node:assert/strict';
const {ChatStore}=await import('../../../services/chat-bridge/src/store.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {parseOperationId}=await import('../../../packages/contracts/src/operation-id.ts');
const record={object:{id:'object',parentId:'root',contractKey:'chat-bridge/conversation',effectivelyArchived:false},revision:{revision:1,contractVersion:'1.3.0',mediaType:'application/json'},content:{encoding:'json',value:{sequence:1}}};
const cases=[];
function add(label,edit=()=>{}){const c=structuredClone({label,action:'read',key:'chat-bridge/conversation',parent:'root',name:'name',pinned:true,record,page:{items:[{objectId:'object'}],nextCursor:null},writeResult:{object:{id:'object'},revision:{revision:2}},value:{sequence:1},create:true,error:null,repeat:1});edit(c);cases.push(c);}
add('pinned-read');add('current-read',c=>c.pinned=false);add('legacy-version',c=>c.record.revision.contractVersion='1.2.0');
add('wrong-contract',c=>c.record.object.contractKey='other');add('wrong-version',c=>c.record.revision.contractVersion='2.0.0');
add('wrong-media',c=>c.record.revision.mediaType='text/plain');add('wrong-encoding',c=>c.record.content.encoding='text');
add('wrong-parent',c=>c.record.object.parentId='other');add('archived',c=>c.record.object.effectivelyArchived=true);add('wrong-pin',c=>c.record.revision.revision=2);
add('named',c=>c.action='named');add('named-null-root',c=>{c.action='named';c.parent=null;c.record.object.parentId=null;});
add('named-absent',c=>{c.action='named';c.page.items=[];});add('named-multiple',c=>{c.action='named';c.page.items.push({objectId:'other'});});
add('named-cursor',c=>{c.action='named';c.page.nextCursor='more';});
add('available',c=>c.action='available');add('available-archived',c=>{c.action='available';c.record.object.effectivelyArchived=true;});
add('available-wrong-pin',c=>{c.action='available';c.record.revision.revision=2;});
add('write-created',c=>c.action='write');add('write-updated',c=>{c.action='write';c.create=false;});
add('write-scoped-replay',c=>{c.action='write';c.repeat=2;});
add('write-input-references',c=>{c.action='write';c.key='chat-bridge/input';c.value={state:'accepted',payload:{images:[{object:{objectId:'image',revision:3}}]},result:{objectId:'result',revision:4}};});
add('write-result-references',c=>{c.action='write';c.key='chat-bridge/result';c.value={textParts:[{object:{objectId:'one',revision:1}},{object:{objectId:'two',revision:2}}]};});
add('write-native-reply-references',c=>{c.action='write';c.key='chat-bridge/reply';c.value={origin:{kind:'native',result:{objectId:'result',revision:1}},artifacts:[{object:{objectId:'artifact',revision:2}}]};});
add('write-other-reply-references',c=>{c.action='write';c.key='chat-bridge/reply';c.value={origin:{kind:'notice'},artifacts:[]};});
const conflict={method:'objects.write',code:'revision_conflict',message:'Synthetic conflict.',outcome:'not_executed'};
add('create-conflict-recovered',c=>{c.action='write';c.error=conflict;});
add('create-conflict-changed-content',c=>{c.action='write';c.error=conflict;c.record.content.value={sequence:2};});
add('create-conflict-changed-revision',c=>{c.action='write';c.error=conflict;c.record.revision.revision=2;});
add('create-conflict-absent',c=>{c.action='write';c.error=conflict;c.page.items=[];});
add('update-conflict-propagates',c=>{c.action='write';c.error=conflict;c.create=false;});
add('unknown-write-not-retried',c=>{c.action='write';c.error={...conflict,code:'outcome_unknown',outcome:'unknown'};});
add('read-owner-error',c=>c.error={method:'objects.read',code:'service_unavailable',message:'Synthetic outage.',outcome:'unknown'});
for(const c of cases){
 const calls=[];
 const client={async request(method,params){calls.push({method,params});if(c.error?.method===method)throw new IvyError(c.error.code,c.error.message,c.error.outcome);switch(method){case 'system.status':return {runtimeEpoch:'chat'};case 'objects.read':return structuredClone(c.record);case 'objects.query':return structuredClone(c.page);case 'objects.write':return structuredClone(c.writeResult);default:throw Error('Unexpected '+method);}}};
 const store=new ChatStore(client,c.parent);let result=[];
 try{for(let i=0;i<c.repeat;i++){
  let value;switch(c.action){
   case 'read':value=await store.read(c.key,c.pinned?{objectId:'object',revision:1}:'object');break;
   case 'named':value=await store.named(c.key,c.name);break;
   case 'available':await store.requireAvailable({objectId:'object',revision:1});value=null;break;
   case 'write':{const before=store.changeVersion;const pin=await store.write(c.key,c.value,'fixture',c.create?{create:{parentId:c.parent,name:c.name}}:{objectId:'object',expectedRevision:1});value={pin,replayed:store.changeVersion===before};break;}
  }result.push(value);
 }}catch(error){const e=IvyError.from(error);result={error:{code:e.code,message:e.message,outcome:e.outcome}};}
 let scoped;
 for(const call of calls){if(call.method==='objects.write'){assert.equal(parseOperationId(call.params.mutationId).runtimeEpoch,'chat');scoped??=call.params.mutationId;assert.equal(call.params.mutationId,scoped);call.params.mutationId='<scoped>';}}
 c.expected={result,calls};store.close();
}
process.stdout.write(JSON.stringify(cases));
