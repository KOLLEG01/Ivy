import {environment,now,instructions,noticeInstructions,starting,operation,clone,at,result} from './chat-main-oracle.mjs';
import {extension as deliveryExtension} from './chat-delivery-oracle.mjs';
const {ChatBridge}=await import('../../../services/chat-bridge/src/bridge.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const abortReason=()=>new IvyError('fixture_provider_closed','Original provider invocation lost its connection.','unknown',{text:'retained-\ud800'});
function matches(record,p){
 if(p.op==='and')return p.args.every(v=>matches(record,v));
 if(p.op==='or')return p.args.some(v=>matches(record,v));
 let value;if(p.field.startsWith('object.'))value=record.object[p.field.slice(7)];
 else if(p.field.startsWith('data:/')){value=record.content.value;for(const part of p.field.slice(6).split('/'))value=value?.[part.replaceAll('~1','/').replaceAll('~0','~')];}
 if(p.op==='isNull')return value==null;
 if(p.op==='gt')return value>p.value;
 if(p.op==='lte')return value<=p.value;
 if(p.op==='eq')return JSON.stringify(value)===JSON.stringify(p.value);
 throw Error('Unsupported synthetic provider query predicate '+p.op);
}

function extension(){
 const base=deliveryExtension();let bridge,controller;
 async function request(s,c){
  const caller=s.caller??'alice',name=s.name,label=s.label??'provider-'+name;
  const expectedBridge=c.main.admission.expected(caller),channel=clone(c.definition.channels[0].channel);
  const m=await c.main.find(),input=c.records.find(r=>r.object.contractKey==='chat-bridge/input');
  const common={action:name,operationId:operation(label),expectedBridge};
  let args;
  switch(name){
   case'workspace':args={};break;
   case'createMain':args={...common,expectedMainRevision:s.current?m?.pin.revision??null:null,reason:'Explicit synthetic Main creation.'};break;
   case'send':args={...common,channel,messageId:label,expectedBinding:m?.value.binding??null,payload:{text:'Original queued input.',images:[]}};break;
   case'cancel':args={...common,inputId:input?.object.id??'missing-input',expectedRevision:input?.revision.revision??1,reason:'Explicit synthetic cancellation.'};break;
   case'receive':args={...common,channel,afterSequence:0,limit:8};break;
   case'acknowledge':{const receive=await c.ops.find(s.receiveCaller??caller,operation(s.receiveLabel??'provider-receive'));const offer=receive?.value.outcome?.offer;args={...common,offerId:offer?.object.objectId??'missing-offer',replyIds:offer?.data.replies.slice(0,1).map(v=>v.objectId)??['missing-reply']};break;}
   case'operation':args={expectedBridge,operationId:operation(s.operationLabel??'provider-send')};break;
   case'history':args={expectedBridge,channel,afterSequence:0,limit:8};break;
   case'input':args={expectedBridge,inputId:input?.object.id??'missing-input'};break;
   case'notify':args={action:name,operationId:operation(label),source:{objectId:'synthetic-source',revision:1},text:'Original private context.'};break;
   case'notice':args={operationId:operation(s.operationLabel??'provider-notify')};break;
   case'retryResult':{const collection=input?await bridge.worker.collector.find(input.object.id):null;args={inputId:input?.object.id??'missing-input',expectedCollection:(s.replay?collection?.value.retryAuthorization?.failed:collection?.pin)??{objectId:'missing-collection',revision:1},expectedBridge,operationId:operation(label),reason:'Explicit original collection retry.'};break;}
   default:throw Error('Unknown provider request '+name);
  }
  if(s.patch)Object.assign(args,clone(s.patch));
  if(s.scopePatch)Object.assign(args.expectedBridge,clone(s.scopePatch));
  if(s.replay&&name==='cancel')args=clone((await c.ops.find(caller,operation(label))).value.request);
  if(Object.hasOwn(s,'args'))args=clone(s.args);
  return{args,caller};
 }
 return{
  ...base,
  opened(c){base.opened(c);bridge=new ChatBridge(c.main);controller=new AbortController();},
  async rpc(method,p,c,next){
   if(method==='objects.query')return{items:c.records.filter(r=>r.object.contractKey===p.contractKey&&(p.includeArchived||!r.object.effectivelyArchived)&&matches(r,p.where)).map(r=>({objectId:r.object.id,revision:r.revision.revision})),nextCursor:null};
   if(method==='objects.read'&&p.revision!==undefined){const current=c.records.find(r=>r.object.id===p.objectId);if(current&&current.revision.revision!==p.revision)throw new IvyError('revision_pruned','Original public revision was pruned.');}
   if(method==='tools.call'&&p.qualifiedName==='agent.status'&&c.controls.nativeReadyError)throw new IvyError('fixture_native_unavailable','Original native status is unavailable.','unknown');
   const value=await base.rpc(method,p,c,next);
   if(method==='tools.call'&&p.qualifiedName==='agent.read'&&p.arguments.method!=='thread/read'&&c.controls.collectionReadError)value.reply={error:{code:-32600,message:'Original synthetic collection read failure.'}};
   if(c.controls.abortAfterRpc===method){delete c.controls.abortAfterRpc;c.events.at(-1).abortAfterRpc=true;controller.abort(abortReason());}
   return value;
  },
  async run(s,c){
   if(s.action==='provider'){
    const {args,caller}=await request(s,c);
    if(s.abortBefore)controller.abort(abortReason());
    if(s.abortAfterRpc)c.controls.abortAfterRpc=s.abortAfterRpc;
    const operationId=Object.hasOwn(s,'transportId')?s.transportId:args?.operationId;
    return bridge.invoke(s.name,args,{callerPrincipalId:caller,generation:s.generation??7,operationId,signal:controller.signal});
   }
   if(s.action==='provider-worker'){await bridge.worker.step();return;}
   return base.run(s,c);
  },
 };
}

const seeds={},cases=[],versions=Object.keys(starting);
for(const version of versions){
 seeds['empty-'+version]=clone(starting[version]);
 const e=environment(starting[version],false,extension());try{
  if((await e.run({action:'provider',name:'createMain'})).phase!=='succeeded')throw Error('Original provider seed creation failed');
  seeds['bound-'+version]=e.exportSeed();
 }finally{e.close();}
}
for(const kind of ['queued','replies','failed-collection','notice']){
 const e=environment(seeds['bound-0.154.0'],false,extension());try{
  const name=kind==='notice'?'notify':'send',accepted=await e.run({action:'provider',name,...(kind==='notice'?{caller:'producer'}:{})});
  if(kind!=='notice'&&accepted.phase!=='succeeded')throw Error('Original provider input seed did not queue '+JSON.stringify({kind,accepted,main:(await e.main.find()).value}));
  if(kind==='replies')await e.run({action:'provider',name:'send',label:'provider-tail'});
  if(kind!=='queued'){
   if(kind==='failed-collection'){await e.run({action:'provider-worker'});await e.run({action:'provider-worker'});await e.run({action:'state',value:{collectionReadError:true}});}
   else await e.run({action:'state',value:{turnPhase:'completed'}});
   for(let n=0;n<(kind==='replies'?24:9);n++){const v=await result(()=>e.run({action:'provider-worker'}));if(kind!=='failed-collection'&&v.error)throw Error('Original provider worker seed '+kind+' step'+n+JSON.stringify(v));}
  }
  if(kind==='replies'&&!(await e.snapshot(null)).records.some(r=>r.object.contractKey==='chat-bridge/reply')){const probe=await e.snapshot(null);throw Error('Original provider reply seed stalled '+JSON.stringify({main:(await e.main.find()).value,records:probe.records.map(r=>({key:r.object.contractKey,value:r.content.value})),collections:probe.revisions.filter(r=>r.key==='chat-bridge/collection')}));}
  seeds[kind]=e.exportSeed();
 }finally{e.close();}
}
const api=(name,extra={})=>({action:'provider',name,...extra}),reopen={action:'reopen'},state=value=>({action:'state',value}),w={action:'provider-worker'};
const add=(label,steps,seed='bound-0.154.0')=>cases.push({label,steps,seed});
for(const version of versions){
 add('all-catalog-create-send-'+version,[api('workspace'),api('createMain'),api('workspace'),api('send'),api('operation'),api('input'),api('history'),reopen,api('send'),api('operation')],'empty-'+version);
 add('all-catalog-notice-'+version,[api('notify',{caller:'producer'}),api('notice',{caller:'producer'}),api('operation',{caller:'producer',operationLabel:'provider-notify'}),reopen,api('notify',{caller:'producer'}),api('notice',{caller:'producer'})],'bound-'+version);
}
add('completed-replies-offer-partial-ack-replay',[api('history'),api('receive'),api('acknowledge'),api('history'),reopen,api('receive'),api('acknowledge'),api('operation',{operationLabel:'provider-acknowledge'})],'replies');
add('notice-reply-and-operation-retain-producer-scope',[api('notice',{caller:'producer'}),api('notice',{caller:'alice'}),api('operation',{caller:'alice',operationLabel:'provider-notify'}),api('operation',{caller:'producer',operationLabel:'provider-notify'}),api('history')],'notice');
add('queued-cancellation-through-provider',[api('cancel'),api('operation',{operationLabel:'provider-cancel'}),w,w,api('input'),reopen,api('cancel',{replay:true}),api('operation',{operationLabel:'provider-cancel'})],'queued');
add('failed-original-collection-retry-and-replay',[state({collectionReadError:false,turnPhase:'completed'}),api('retryResult'),api('retryResult',{replay:true}),w,w,w,api('history'),api('input')],'failed-collection');
add('retry-keeps-current-collection-pin',[api('retryResult',{patch:{expectedCollection:{objectId:'foreign',revision:1}}})],'failed-collection');
add('retry-does-not-become-operation-receipt',[api('retryResult'),api('operation',{operationLabel:'provider-retryResult'})],'failed-collection');
add('workspace-preserves-saved-state-during-native-outage',[api('workspace'),state({nativeReadyError:true}),api('workspace'),api('history'),state({nativeReadyError:false}),api('workspace')]);
add('workspace-refuses-own-generation-outage',[state({node:{connected:false}}),api('workspace'),api('history')]);
for(const field of ['project','nativePlan','primary'])add('workspace-checks-binding-'+field,[{action:'patch-binding',field},api('workspace')]);
for(const generation of [6,8])add('stale-generation-before-schema-'+generation,[api('send',{generation,args:null}),api('workspace',{generation})]);
for(const name of ['createMain','send','cancel','receive','acknowledge','notify','retryResult']){
 add('mutation-transport-id-'+name,[api(name,{transportId:'chat:'+now+':another'}),api(name,{transportId:null})],name==='cancel'?'queued':name==='retryResult'?'failed-collection':'bound-0.154.0');
 add('schema-before-operation-read-'+name,[api(name,{args:{}}),api(name,{args:null})]);
}
for(const name of ['workspace','operation','history','input','notice'])add('read-schema-'+name,[api(name,{args:{unexpected:true}}),api(name,{args:null})]);
for(const key of ['principalId','workspaceId','callerPrincipalId'])add('caller-scope-before-content-'+key,[api('history',{scopePatch:{[key]:'foreign'}}),api('input',{scopePatch:{[key]:'foreign'}}),api('operation',{scopePatch:{[key]:'foreign'}})],'queued');
add('operation-is-caller-scoped',[api('operation',{caller:'bob'}),api('operation',{caller:'alice'})],'queued');
add('missing-operation-does-not-read-another-caller',[api('operation',{operationLabel:'missing'})]);
add('operation-action-conflict',[api('notify',{label:'provider-send'})],'queued');
for(const operationId of ['malformed','old:'+now+':expired','chat:'+(now+60001)+':future'])add('new-operation-epoch-or-time-'+operationId,[api('send',{patch:{operationId}})]);
add('pre-abort-precedes-generation-and-schema',[api('send',{abortBefore:true,generation:6,args:null})]);
add('post-abort-retains-original-send',[api('send',{abortAfterRpc:'objects.write'}),reopen,api('operation'),api('send'),api('input')]);
add('post-abort-retains-workspace-reason',[api('workspace',{abortAfterRpc:'serviceNodes.get'})]);

for(const c of cases){const e=environment(seeds[c.seed],false,extension());c.expected=[];try{for(const s of c.steps)c.expected.push(await e.snapshot(await result(()=>e.run(s))));}finally{e.close();}}
process.stdout.write(JSON.stringify({now,instructions,noticeInstructions,seeds,cases,boundary:'Original ChatBridge and domain/worker owners execute. Only synthetic Hive/native replies, time and provider context fields/abort are controlled. No account effects or new approval exception.'}));
for(const c of cases.filter(c=>/^(all-catalog-create-|completed-replies-|queued-cancellation-|failed-original-collection-)/.test(c.label)))if(c.expected.some(s=>s.output.error))throw Error('Original successful provider case failed '+c.label+JSON.stringify(c.expected.map(s=>s.output)));
const failed=seeds['failed-collection'];
if(!failed||!cases.find(c=>c.label==='failed-original-collection-retry-and-replay').expected[1].output.value?.retryAuthorization)throw Error('Original collection retry authorization not exercised');
if(cases.find(c=>c.label==='workspace-preserves-saved-state-during-native-outage').expected[2].output.value?.nativeOwnerReady!==false)throw Error('Native outage did not preserve saved workspace');
