import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import {syncBuiltinESMExports} from 'node:module';
import {load,rootId,itemId,sourceId,operationId,at,id,caller,scope,definition,source,message,policy,settings,owner,assessment,item,accepted} from './inbox-fixtures.mjs';
const {SecretaryEngine,operationName,sourceName,identityHash,messageHash}=await load('services/secretary/src/engine.js');
const {SecretaryStore}=await load('services/secretary/src/store.js');
const {IvyError,canonical,hashJson}=await load('packages/sdk/src/node.js');
const wall=1800000000000,uuid='00000000-0000-4000-8000-000000000fff';
Date.now=()=>wall;crypto.randomUUID=()=>uuid;syncBuiltinESMExports();
const gap={contractKey:'secretary/media-manifest',pin:null,contentHash:null,name:'attachment',mediaType:'application/octet-stream',errorCode:'gap'};
const artifactId='00000000-0000-4000-8000-000000000005',taskId='00000000-0000-4000-8000-000000000006';
const attachment={...gap,pin:{objectId:artifactId,revision:1},contentHash:'sha256:'+'a'.repeat(64),errorCode:null};
const captureId=`fixture-epoch:${wall}:completed-capture`;
const clone=structuredClone,rows=[],templates={};
async function add(label,c={}) {
 const action=c.action??'capture',method=c.method??'action';
 const config=clone(c.settings??settings);
 const request={operationId:id,expectedScope:scope,action,
  ...(action==='capture'?{sourceId:'mail',message:{...message,...c.message}}:
   action==='assess'?{item:{objectId:itemId,revision:1},assessment:{...assessment,...c.assessment}}:
   action==='attach'?{sourceId:'mail',item:{objectId:itemId,revision:1},media:c.media??[gap]}:
   {sourceId:'mail',previousCursor:null,nextCursor:'next',captures:c.captures??[]}),...c.request};
 const input={label,method,caller:c.caller??caller,request,scope:c.scope??scope,settings:config,wall,time:c.time??Date.parse(at),expiredBefore:c.expiredBefore??0,cancelled:c.cancelled??false,repeats:c.repeats??1,locals:[],workflow:!!c.workflow};
 const sourceValue=clone(c.source??{...source,definition:config.sources[0]??definition,definitionHash:hashJson(config.sources[0]??definition)});
 input.locals.push({id:sourceId,key:'secretary/source',name:sourceName('mail'),value:sourceValue,first:sourceValue});
 for(const [index,extra] of config.sources.entries())if(extra.sourceId!=='mail'){
  const value={...source,definition:extra,definitionHash:hashJson(extra)};
  input.locals.push({id:`00000000-0000-4000-8000-${String(100+index).padStart(12,'0')}`,key:'secretary/source',name:sourceName(extra.sourceId),value,first:value});
 }
 if(c.prior){const first=clone(templates[c.prior]),value={...first,phase:c.priorPhase??'accepted',...c.priorChange};input.locals.push({id:operationId,key:'secretary/operation',name:operationName(caller,first.operationId),value,first});}
 if(c.captureState){
  const originalRequest={action:'capture',operationId:captureId,expectedScope:scope,sourceId:'mail',message};
  const first={...accepted,operationId:captureId,requestHash:hashJson(originalRequest),request:null,ignoredReason:'outgoing',captureIdentity:null};
  const value={...first,phase:c.captureState,...c.captureChange};
  input.locals.push({id:operationId,key:'secretary/operation',name:operationName(caller,captureId),value,first});
 }
 let store,current=null,writeFailed=false;
 const trace=[],steps=[];
 const raw=(value,revision=1)=>({object:{id:itemId,parentId:rootId,name:'item-'+value.identityHash.slice(7),effectivelyArchived:false,contractKey:'secretary/item'},revision:{revision,contractVersion:'1.3.0',mediaType:'application/json',contentHash:hashJson(value),byteLength:Buffer.byteLength(canonical(value)),references:store.objectWriteRequest('secretary/item',value,{object:{objectId:itemId,revision}},id).references,createdAt:at},content:{encoding:'json',value}});
 const client={async request(method,p,options){
  trace.push({method,params:clone(p),timeout:options?.timeoutMs??35000});
  try{
   if(trace.length===c.failAt)throw new IvyError(c.failCode??'fixture_failure','Fixture failure.',c.outcome??'unknown');
   let result;
   if(method==='serviceNodes.get')result={...owner,...c.owner};
   else if(method==='system.status')result={runtimeEpoch:'fixture-epoch'};
   else if(method==='serviceNodes.list')result={items:[],nextCursor:null};
   else if(method==='objects.query')result={items:current?[{objectId:itemId,revision:current.revision.revision}]:[],nextCursor:null};
   else if(method==='objects.read'){
    if(p.objectId===artifactId)result='mediaRaw' in c?c.mediaRaw:{object:{id:artifactId,contractKey:attachment.contractKey,ownerObjectId:itemId,effectivelyArchived:false,...c.mediaObject},revision:{contentHash:attachment.contentHash,...c.mediaRevision}};
    else if(p.objectId===taskId){const value={saved:true},content={encoding:'json',value,...c.taskContent};result='taskRaw' in c?c.taskRaw:{object:{id:taskId,contractKey:'task-board/task',effectivelyArchived:false,...c.taskObject},revision:{contractVersion:'1.0.0',contentHash:hashJson(value),...c.taskRevision},content};}
    else {if(!current||p.objectId!==itemId||(p.revision!==undefined&&p.revision!==current.revision.revision))throw new IvyError('not_found','Fixture object missing.');result=current;}
   }else if(method==='objects.write'){
    const fail=c.writeFailure&&!writeFailed;writeFailed=true;
    if(fail&&!c.afterWrite)throw new IvyError(c.writeCode??'fixture_write_failure','Fixture write failure.',c.outcome??'unknown');
    if(p.objectId&&current?.revision.revision!==p.expectedRevision)throw new IvyError('revision_conflict','Fixture revision changed.');
    if(p.create&&current)throw new IvyError('mutation_conflict','Fixture name exists.');
    current=raw(clone(p.content.value),p.create?1:p.expectedRevision+1);current.revision.references=p.references;
    if(fail)throw new IvyError(c.writeCode??'fixture_write_failure','Fixture write failure.',c.outcome??'unknown');
    result={object:{id:itemId},revision:{revision:current.revision.revision}};
   }else throw Error(`Unexpected RPC ${method}`);
   for(const path of p.objectId===taskId?c.taskOmit??[]:p.objectId===artifactId?c.mediaOmit??[]:[]){const parts=path.split('.'),key=parts.pop(),parent=parts.reduce((v,k)=>v[k],result);delete parent[key];}
   steps.push({method,value:clone(result)});return clone(result);
  }catch(cause){const e=IvyError.from(cause);steps.push({method,error:{code:e.code,message:e.message,outcome:e.outcome}});throw cause;}
 }};
 store=new SecretaryStore(client,rootId);
 if(c.item!==null&&(action==='assess'||action==='attach'||c.item)){
  let value={...clone(item),...c.item};
  if(c.itemMessage){value.message={...message,...c.itemMessage};value.identityHash=identityHash(value.sourceId,value.message);value.messageHash=messageHash(value.message);}
  current=raw(value,c.itemRevision??1);
 }
 for(const row of input.locals){const revision=hashJson(row.first)===hashJson(row.value)?1:2;store.db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(row.id,row.key,row.name,revision,0,row.value.phase&&row.value.phase!=='accepted'?1:null,canonical(row.value));store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(row.id,1,canonical(row.first));if(revision===2)store.db.prepare('INSERT INTO revisions VALUES(?,?,?)').run(row.id,2,canonical(row.value));}
 if(c.workflow)store.technicalCreate('secretary/public-operation',operationName(caller,id),{fixture:true});
 if(input.expiredBefore)store.db.prepare('INSERT INTO journal_state VALUES(?,?)').run('expiredBefore',String(input.expiredBefore));
 const abort=new AbortController();if(input.cancelled)abort.abort(new IvyError('fixture_cancelled','Fixture cancelled.'));
 const engine=Object.assign(Object.create(SecretaryEngine.prototype),{client,store,settings:config,epoch:Promise.resolve('fixture-epoch'),owner:{serviceNodeId:'fixture.secretary'},signal:abort.signal,now:()=>new Date(input.time)});
 const results=[];
 for(let n=0;n<input.repeats;n++)try{results.push({value:await(method==='operation'?engine.operation(input.caller,input.scope,id):engine.action(input.caller,clone(request)))});}catch(cause){const e=IvyError.from(cause);results.push({code:e.code,message:e.message,outcome:e.outcome});}
 const retained=store.db.prepare('SELECT id,key,name,current_revision,value_json,completed_at FROM documents ORDER BY key,name').all().map(row=>({key:row.key,name:row.name,revision:row.current_revision,value:JSON.parse(row.value_json),completed:row.completed_at!==null,history:store.db.prepare('SELECT revision,value_json FROM revisions WHERE id=? ORDER BY revision').all(row.id).map(r=>({revision:r.revision,value:JSON.parse(r.value_json)}))}));
 if(c.template){const row=retained.find(r=>r.key==='secretary/operation'&&r.name===operationName(caller,id));assert.ok(row);templates[c.template]=clone(row.history[0].value);}
 const acknowledgements=store.db.prepare('SELECT capture_id FROM capture_acknowledgements ORDER BY capture_id').all().map(r=>r.capture_id);
 store.close();input.steps=steps;rows.push({input,expected:{results,trace,retained,acknowledgements}});
}
await add('fresh capture and exact replay',{repeats:2,template:'capture'});
await add('fresh ignored capture and exact replay',{message:{outgoing:true},repeats:2,template:'ignored'});
await add('fresh assessment and exact replay',{action:'assess',repeats:2});
await add('fresh attachment and exact replay',{action:'attach',repeats:2});
await add('fresh checkpoint and exact replay',{action:'checkpoint',repeats:2});
for(const action of ['capture','assess','attach','checkpoint']){
 await add(action+' invalid shape',{action,request:{unexpected:true}});
 await add(action+' scope conflict before epoch',{action,request:{expectedScope:{...scope,secretaryId:'other'},operationId:'bad'}});
 await add(action+' wrong epoch before owner',{action,request:{operationId:`other:${wall}:inbox`},cancelled:true});
 await add(action+' expired new',{action,expiredBefore:wall+1});
 await add(action+' future new',{action,request:{operationId:`fixture-epoch:${wall+60001}:inbox`}});
 await add(action+' cancelled',{action,cancelled:true});
 await add(action+' owner mismatch',{action,owner:{synced:false}});
 await add(action+' assignment operation collision',{action,workflow:true});
 await add(action+' invalid observation clock',{action,time:8640000000000001});
}
await add('capture forbidden',{caller:'other'});
await add('capture unknown source',{request:{sourceId:'missing'}});
await add('capture account mismatch before owner',{message:{accountId:'other'},cancelled:true});
for(const stamp of ['2026-02-30T00:00:00.000Z','2026-01-01T00:00:00Z','2026-01-01T00:00:00.000+00:00','+275760-09-13T00:00:00.000Z','0000-01-01T00:00:00.000Z','not-a-date'])await add('capture exact instant '+stamp,{message:{observedAt:stamp}});
for(const url of ['https://example.invalid/path','http:example.invalid','https:/example.invalid',' HTTPS://example.invalid\t/path\n','http://127.1/path','https://[::1]/','https://example.invalid/\ud800','https://\ud800.invalid','https://user:pass@example.invalid/','https://:pass@example.invalid/','https://:@example.invalid/','//example.invalid','relative','file:///tmp/a','mailto:sender@example.invalid','https://example.invalid:99999/','https://example.invalid/%zz','https://example.invalid/#x','http://0xffffffff/','https://example.invalid\\x'])await add('capture URL '+JSON.stringify(url),{message:{url}});
await add('capture own sender',{message:{senderId:'self'}});
await add('capture sender deny list',{settings:{...settings,sources:[{...definition,allowedSenderIds:[]}]}});
await add('capture lone-surrogate sender exact ignore',{message:{senderId:'\ud800'},settings:{...settings,sources:[{...definition,ownSenderIds:['\ud800']}]}});
await add('capture duplicate keeps original observation time',{item, message:{observedAt:'2026-01-02T00:00:00.000Z'}});
await add('capture duplicate changed message',{item, message:{text:'changed'}});
await add('retained capture with expired ring',{prior:'capture',expiredBefore:wall+1,repeats:2});
await add('retained capture conflict',{prior:'capture',message:{text:'changed'}});
await add('retained ignored request conflict',{prior:'ignored',message:{outgoing:true,text:'changed'}});
await add('retained capture prior failure',{prior:'capture',priorPhase:'failed',priorChange:{errorCode:'prior_failure'}});
await add('retained capture changed original bytes',{prior:'capture',priorChange:{requestHash:'sha256:'+'0'.repeat(64)}});
for(const patch of [{disposition:'ignore',notification:'none'},{disposition:'record',notification:'none'},{notification:'needs_attention'},{urgency:'low'},{urgency:'high'},{disposition:'record',notification:'main'},{disposition:'notify',notification:'none'},{disposition:'task',task:null},{task:{objectId:taskId,revision:1}}])await add('assessment disposition '+JSON.stringify(patch),{action:'assess',assessment:patch});
await add('assessment minimum urgency suppression',{action:'assess',settings:{...settings,policy:{...policy,minimumMainUrgency:'critical'}}});
await add('assessment stale item',{action:'assess',request:{item:{objectId:itemId,revision:9}}});
await add('assessment item missing',{action:'assess',item:null});
for(const options of [{},{taskObject:{effectivelyArchived:true}},{taskObject:{contractKey:'other'}},{taskRevision:{contractVersion:'1.1.0'}},{taskRevision:{contentHash:'sha256:'+'0'.repeat(64)}},{taskContent:{encoding:'text'}}])await add('assessment exact task '+JSON.stringify(options),{action:'assess',assessment:{disposition:'task',task:{objectId:taskId,revision:1}},...options});
for(const options of [{taskRaw:null},{taskRaw:{}},{taskRaw:{object:null}},{taskRaw:{object:{effectivelyArchived:true}}},{taskRaw:{object:{contractKey:'other'}}},{taskRaw:{object:{contractKey:'task-board/task'}}},{taskOmit:['content']},{taskOmit:['content.value']},{taskRevision:{contractVersion:'other'},taskOmit:['content']}])await add('assessment boundary order '+JSON.stringify(options),{action:'assess',assessment:{disposition:'task',task:{objectId:taskId,revision:1}},...options});
for(const urgency of ['normal','critical'])await add('assessment no Voice target '+urgency,{action:'assess',assessment:{notification:'voice',urgency}});
const voiceSettings={...settings,policy:{...policy,voiceEscalation:{serviceNodeId:'fixture.phone',recipientId:'recipient',requestDefinitionHash:'sha256:'+'1'.repeat(64)}}};
for(const urgency of ['normal','critical'])await add('assessment configured Voice '+urgency,{action:'assess',assessment:{notification:'voice',urgency},settings:voiceSettings,repeats:2});
for(const media of [[gap,gap],[{...gap,pin:attachment.pin}],[{...gap,contentHash:attachment.contentHash}],[{...attachment,errorCode:'gap'}],[attachment]])await add('attachment evidence '+JSON.stringify(media),{action:'attach',media});
await add('attachment duplicate retained media',{action:'attach',item:{media:[gap]},media:[gap]});
await add('attachment total media limit',{action:'attach',item:{media:Array.from({length:64},(_,n)=>({...gap,name:'old-'+n}))}});
await add('attachment stale pin',{action:'attach',request:{item:{objectId:itemId,revision:9}}});
await add('attachment wrong source',{action:'attach',request:{sourceId:'other'},settings:{...settings,sources:[definition,{...definition,sourceId:'other'}]}});
for(const options of [{mediaObject:{effectivelyArchived:true}},{mediaObject:{ownerObjectId:sourceId}},{mediaObject:{contractKey:'other'}},{mediaRevision:{contentHash:'sha256:'+'0'.repeat(64)}}])await add('attachment mismatched artifact '+JSON.stringify(options),{action:'attach',media:[attachment],...options});
for(const options of [{mediaRaw:null},{mediaRaw:{}},{mediaRaw:{object:null}},{mediaRaw:{object:{contractKey:'other'}}},{mediaObject:{effectivelyArchived:true},mediaOmit:['revision']},{mediaOmit:['revision']}])await add('attachment boundary order '+JSON.stringify(options),{action:'attach',media:[attachment],...options});
await add('checkpoint no advancement',{action:'checkpoint',request:{previousCursor:'next'}});
await add('checkpoint duplicate capture IDs',{action:'checkpoint',captures:[captureId,captureId]});
await add('checkpoint missing capture',{action:'checkpoint',captures:[captureId]});
for(const captureState of ['accepted','failed','succeeded'])await add('checkpoint capture '+captureState,{action:'checkpoint',captures:[captureId],captureState,repeats:2});
await add('checkpoint changed cursor',{action:'checkpoint',source:{...source,cursor:'other'}});
for(const outcome of ['not_executed','unknown','completed'])for(const afterWrite of [false,true])for(const action of ['capture','assess','attach'])await add(action+' lost effect '+outcome+'/'+afterWrite,{action,writeFailure:true,outcome,afterWrite,repeats:2});
for(const failAt of [1,2,3,4,5,6])await add('capture RPC failure boundary '+failAt,{failAt,repeats:2});
for(const prior of [null,'capture','ignored'])await add('read caller operation '+prior,{method:'operation',prior});
await add('read operation scope conflict',{method:'operation',scope:{...scope,secretaryId:'other'},prior:'capture'});
await add('read operation wrong caller',{method:'operation',caller:'other',prior:'capture'});
assert.equal(rows.length,166);
for(const row of rows.slice(0,5))assert.ok(row.expected.results.every(r=>r.value?.phase==='succeeded'),row.input.label);
assert.ok(rows.find(r=>r.input.label==='assessment configured Voice critical').expected.results.every(r=>r.value?.phase==='succeeded'));
assert.deepEqual(rows.find(r=>r.input.label==='checkpoint capture succeeded').expected.acknowledgements,[operationId]);
console.log(JSON.stringify({rootId,epoch:'fixture-epoch',uuid,rows}));
