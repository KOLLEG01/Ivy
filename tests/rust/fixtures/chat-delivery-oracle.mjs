import {environment,now,instructions,noticeInstructions,starting,operation,clone,at,result} from './chat-main-oracle.mjs';
const {ChatDelivery}=await import('../../../services/chat-bridge/src/delivery.ts');
const {ChatWorker}=await import('../../../services/chat-bridge/src/worker.ts');
const {ChatNotices}=await import('../../../services/chat-bridge/src/notices.ts');
const {deriveOperationId}=await import('../../../packages/contracts/src/operation-id.ts');
const {hashJson,digest}=await import('../../../packages/contracts/src/canonical.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const {resolveChatPlan}=await import('../../../services/chat-bridge/src/native-plan.ts');

function extension(){
 let delivery,worker,reads=0,turns=0;
 const thread=(c,id)=>({cliVersion:c.source.catalog.version,createdAt:1788690000,cwd:c.controls.readCwd??'/synthetic/project',ephemeral:c.controls.readEphemeral??false,id:c.controls.readThreadId??id,modelProvider:'openai',preview:'Synthetic Main',projectId:null,sessionId:id,source:'appServer',status:{type:c.controls.threadStatus??'idle',...(c.controls.threadStatus==='active'?{activeFlags:[]}: {})},turns:[],updatedAt:1788690000,canAcceptDirectInput:c.controls.directInput??true,name:'Synthetic'});
 const firstInput=async c=>{const m=await c.main.find();return c.store.read('chat-bridge/input',m.value.queue[0].inputId);};
 function observed(c,args,reply){const id='delivery-read-'+(++reads);return{schemaVersion:1,observationId:id,serviceNodeId:'owner',callerPrincipalId:'chat-service',nativeVersion:c.source.catalog.version,nativeExecutableHash:c.source.catalog.nativeExecutableHash,catalogHash:c.source.catalogHash,epoch:c.controls.epoch,requestId:id+'-request',observedAt:at,...clone(args),requestHash:hashJson({method:args.method,params:args.params}),reply};}
 function operationRecord(c,args,reply,phase='succeeded'){
  return{schemaVersion:1,operationId:args.operationId,callerPrincipalId:'chat-service',serviceNodeId:'owner',nativeVersion:c.source.catalog.version,nativeExecutableHash:c.source.catalog.nativeExecutableHash,method:args.method,params:clone(args.params),requestHash:hashJson({method:args.method,params:args.params}),phase,createdAt:at,updatedAt:at,epoch:c.controls.epoch,requestId:'delivery-native-'+(c.journal.size+1),code:phase==='failed'?'native_error':null,reply:phase==='dispatched'?null:reply};
 }
 function status(){return{idle:worker.idle,runnable:worker.runnable,turnId:worker.turnId};}
 return{
  opened(c){delivery=new ChatDelivery(c.main);worker=new ChatWorker(c.main);},
  async beforeRequest(method,p,event,c){
   if(method!=='tools.call'||p.qualifiedName!=='agent.invoke'||p.arguments.method==='thread/start')return false;
   const args=p.arguments,main=await c.main.find(),ticket=main.value.queue[0];
   if(!ticket||main.value.pendingAction)throw Error('No current unclaimed queue head before invoke');
   const record=c.records.find(r=>r.object.id===ticket.inputId),input={pin:{objectId:record.object.id,revision:record.revision.revision},value:record.content.value},slot=args.method==='turn/start'?'turn':args.method==='turn/interrupt'?'interrupt':'resume';
   const selected=input.value.nativeCalls[slot];if(!selected)throw Error('Native delivery preceded its saved input attachment');
   const call=await c.store.read('chat-bridge/native-call',selected,input.pin.objectId);
   if(call.value.operationId!==args.operationId||(slot!=='interrupt'&&input.value.cancellation)||(slot==='interrupt'&&!input.value.cancellation))throw Error('The saved original input does not authorize this native call');
   event.assertInputClaim={inputId:input.pin.objectId,slot};return true;
  },
  async rpc(method,p,c,next){
   if(method==='system.diagnostics')return{items:[],nextCursor:null};
   if(method==='tools.call'){
    const name=p.qualifiedName?.split('.').slice(1).join('.'),args=p.arguments;
    if(name==='frameLimits')return{serviceNodeId:'owner',nativeVersion:c.source.catalog.version,nativeExecutableHash:c.source.catalog.nativeExecutableHash,catalogHash:c.source.catalogHash,epoch:c.controls.capacityEpoch??c.controls.epoch,requestFrameBytes:6291456,receivedFrameBytes:25165824,answerFrameBytes:4194304,managementFrameBytes:33554432};
    if(name==='read'){
     if(c.controls.transportReadError)throw new IvyError('fixture_unavailable','Synthetic native read transport unavailable.','unknown');
     let reply;
     if(args.method==='thread/read'){
      if(c.controls.readError)reply={error:{code:c.controls.readErrorCode??-32600,message:c.controls.readError==='own'?'thread not loaded: '+args.params.threadId:c.controls.readError}};
      else reply={result:{thread:thread(c,args.params.threadId)}};
     }else{
      const saved=[...c.journal.values()].filter(v=>v.method==='turn/start').at(-1),turn=saved?.reply?.result?.turn?.id??'delivery-turn-1',phase=c.controls.turnPhase??'inProgress';
      if(args.method==='thread/items/list')reply=c.controls.itemsUnsupported?{error:{code:-32601,message:'Synthetic original unsupported items.'}}:{result:{data:[{turnId:turn,item:{id:'delivery-answer',type:'agentMessage',text:c.controls.answer??'Original delivered reply.\n😀'}}],nextCursor:null}};
      else if(args.method==='thread/turns/list')reply={result:{data:[{id:turn,status:phase,itemsView:args.params.itemsView,items:args.params.itemsView==='full'?[{id:'delivery-answer',type:'agentMessage',text:c.controls.answer??'Original delivered reply.\n😀'}]:[]}],nextCursor:null}};
      else throw Error('Unexpected original delivery read '+args.method);
     }
     if(c.controls.bumpInputOnRead&&args.method==='thread/read'){
      c.controls.bumpInputOnRead=false;const m=await c.main.find(),id=m.value.queue[0].inputId,record=c.records.find(r=>r.object.id===id);
      record.revision.revision++;c.events.at(-1).concurrentInputRevision={inputId:id,revision:record.revision.revision};
     }
     return observed(c,args,reply);
    }
    if(name==='invoke'&&args.method!=='thread/start'){
     if(!c.journal.has(args.operationId)){
      if(args.method==='thread/resume'){c.controls.threadStatus='idle';c.controls.directInput=true;c.controls.readError=null;}
      const reply=args.method==='turn/start'?{result:{turn:{id:'delivery-turn-'+(++turns),status:'inProgress',items:[]}}}:args.method==='turn/interrupt'?{result:{}}:{result:{thread:thread(c,args.params.threadId),cwd:'/synthetic/project',model:'fixture-model',modelProvider:'openai',approvalPolicy:'on-request',approvalsReviewer:'user',sandbox:{type:'readOnly'},reasoningEffort:'high'}};
      const phase=c.controls.nativePhase??'succeeded';
      c.journal.set(args.operationId,operationRecord(c,args,phase==='failed'?{error:{code:-1,message:'Original native input refusal.'}}:reply,phase));
     }
     if(c.controls.loseDeliveryReply){c.controls.loseDeliveryReply=false;throw new IvyError('outcome_unknown','Original successful delivery reply lost.','unknown');}
     return clone(c.journal.get(args.operationId));
    }
    if(name==='prevent'){
     if(!c.journal.has(args.operationId))c.journal.set(args.operationId,{...operationRecord(c,args,null,'failed'),epoch:null,requestId:null,reply:null,code:'native_operation_prevented'});
     return clone(c.journal.get(args.operationId));
    }
   }
   const value=await next();
   if(method==='objects.write'){
    if(p.content.encoding!=='json'){
     const bytes=p.content.encoding==='base64'?Buffer.from(p.content.value,'base64'):Buffer.from(p.content.value);
     value.revision.mediaType=p.create?.contractKey==='chat-bridge/image-png'?'image/png':'text/plain';
     value.revision.byteLength=bytes.length;value.revision.contentHash=digest(bytes);
     c.records.find(r=>r.object.id===value.object.id).revision=clone(value.revision);
    }
    if(c.controls.loseRunningReply&&p.content.encoding==='json'&&p.content.value.state==='running'){
     c.controls.loseRunningReply=false;throw new IvyError('outcome_unknown','Original committed running input reply lost.','unknown');
    }
    if(c.controls.loseResultReply&&p.create?.contractKey==='chat-bridge/result'){
     c.controls.loseResultReply=false;c.events.at(-1).lostResultReply=true;throw new IvyError('outcome_unknown','Original committed Result reply lost.','unknown');
    }
   }
   return value;
  },
  async run(s,c){
   switch(s.action){
    case'delivery':return delivery.step();
    case'worker':return worker.step();
    case'worker-status':return status();
    case'head':{const m=await c.main.find(),id=s.input==='tail'?m.value.queue.at(-1)?.inputId:s.input==='head'?m.value.queue[0]?.inputId:s.input;const h=await delivery.head(id);return h?{input:h.input.pin,binding:h.binding.pin,request:h.request}:null;}
    case'drain':{const descriptor=Object.getOwnPropertyDescriptor(performance,'now');let ticks=0,checks=0;Object.defineProperty(performance,'now',{value:()=>ticks++*(s.millisPerRead??0),configurable:true});try{await worker.drain(()=>++checks<=(s.continueSteps??Number.MAX_SAFE_INTEGER));return{...status(),continueChecks:checks};}finally{if(descriptor)Object.defineProperty(performance,'now',descriptor);else delete performance.now;}}
    case'restart':c.controls.epoch=s.epoch??'delivery-replacement-epoch';c.controls.threadStatus='notLoaded';for(const v of c.journal.values())if(v.phase==='dispatched')Object.assign(v,{phase:'outcome_unknown',code:'native_epoch_lost',reply:null});return;
    case'input':return(await firstInput(c)).value;
    case'pre-prepare-resume':{const input=await firstInput(c),m=await c.main.find(),binding=await c.store.read('chat-bridge/binding',m.value.binding),plan=await resolveChatPlan(c.store,binding.value.nativePlan),origin={predecessor:null,preparedEpoch:null},request={nativeVersion:plan.nativeVersion,catalogSourceHash:plan.catalogSourceHash,method:'thread/resume',params:{...plan.threadResume,threadId:binding.value.primary.nativeId}};await c.main.native.prepare(input.pin.objectId,deriveOperationId(input.value.operationId,'chat:native:resume:'+hashJson(origin)),request,origin);return;}
    case'image-send':{const bytes=Buffer.from(s.valid?'iVBORw0KGgoAAA==':'VGhpcyBpcyBub3QgYSBQTkcgaW1hZ2Uu','base64'),m=await c.main.find(),image=await c.store.client.request('objects.write',{mutationId:'chat:'+now+':synthetic-image',contractVersion:'1.0.0',references:{},create:{parentId:null,ownerObjectId:m.value.conversation.objectId,contractKey:'chat-bridge/image-png',name:'Synthetic original PNG'},content:{encoding:'base64',value:bytes.toString('base64')}});return c.main.queue.send('alice',{action:'send',operationId:operation('image-input'),expectedBridge:c.main.admission.expected('alice'),channel:c.definition.channels[0].channel,messageId:'image-input',expectedBinding:m.value.binding,payload:{text:'',images:[{object:{objectId:image.object.id,revision:1},contentHash:digest(bytes),byteLength:bytes.length,mediaType:'image/png',label:'Synthetic image'}]}});}
    case'cancel-first':{const input=await firstInput(c);const {ChatCancellation}=await import('../../../services/chat-bridge/src/cancellation.ts');return new ChatCancellation(c.main).cancel('alice',{action:'cancel',operationId:operation('delivery-cancel'),expectedBridge:c.main.admission.expected('alice'),inputId:input.pin.objectId,expectedRevision:input.pin.revision,reason:'Original delivery cancellation.'});}
    case'diagnostic-find':{const m=await c.main.find();return m?{pin:m.pin,value:m.value}:null;}
    case'diagnostic-ready':return c.main.native.ready().then(()=>null);
    case'diagnostic-notice':return new ChatNotices(c.main).notice('producer',{operationId:operation('main-notice')});
    case'diagnostic-notify':return new ChatNotices(c.main).notify('producer',{action:'notify',operationId:operation('main-notice'),source:{objectId:'synthetic-source',revision:1},text:'Original private context.'});
    default:throw Error('Unknown delivery action '+s.action);
   }
  },
 };
}
const seeds={},cases=[],versions=Object.keys(starting);
for(const version of versions){
 for(const kind of ['completed','queued']){
  const e=environment(starting[version],false,extension());try{
   if((await e.run({action:'create'})).phase!=='succeeded')throw Error('Delivery Main seed failed');
   if(kind==='queued'){for(const label of ['main-send','tail-send'])if((await e.run({action:'send',label})).phase!=='succeeded')throw Error('Delivery input seed failed');}
   seeds[kind+'-'+version]=e.exportSeed();
  }finally{e.close();}
 }
}
{
 // The resume identity hashes its original predecessor pin. Re-open the exact
 // committed original predecessor instead of masking different random hashes.
 const e=environment(seeds['queued-0.154.0'],false,extension());try{
  await e.run({action:'state',value:{threadStatus:'notLoaded'}});
  for(let n=0;n<3;n++)await e.run({action:'worker'});
  seeds['prepared-original-resume']=e.exportSeed();
 }finally{e.close();}
}
const add=(label,steps,seed='queued-0.154.0')=>cases.push({label,steps,seed});
const w={action:'worker'},d={action:'delivery'},reopen={action:'reopen'},head={action:'head'},status={action:'worker-status'},state=value=>({action:'state',value}),many=(step,n)=>Array.from({length:n},()=>step);
for(const v of versions){
 add('loaded-original-'+v,[head,w,w,status,w,head],'queued-'+v);
 add('unloaded-original-'+v,[state({threadStatus:'notLoaded'}),...many(d,4),{action:'input'}],'queued-'+v);
 add('terminal-worker-drain-'+v,[state({turnPhase:'completed'}),{action:'drain'},status,head],'queued-'+v);
 add('full-read-fallback-'+v,[state({turnPhase:'completed',itemsUnsupported:true}),{action:'drain'},status,head],'queued-'+v);
}
add('busy-before-prepare-and-dispatch',[state({threadStatus:'active'}),w,state({threadStatus:'idle'}),w,state({threadStatus:'active'}),w,state({threadStatus:'idle'}),w,{action:'input'}]);
add('unloaded-after-turn-prepared',[w,{action:'restart'},...many(w,4),{action:'input'}]);
add('same-epoch-unloaded-again',[state({threadStatus:'notLoaded'}),...many(w,4),{action:'input'}],'prepared-original-resume');
add('resume-full-history-keeps-original-request',[{action:'pre-prepare-resume'},state({threadStatus:'notLoaded'}),...many(w,4),{action:'input'}]);
add('only-exact-unloaded-error-allows-resume',[state({readError:'thread not loaded: unrelated'}),w,state({readError:'own'}),...many(w,4),{action:'input'}]);
add('lost-original-native-delivery-reply',[w,state({loseDeliveryReply:true}),w,reopen,w,{action:'input'}]);
add('lost-committed-running-input-reply',[w,state({loseRunningReply:true}),w,reopen,w,{action:'input'}]);
add('unknown-turn-keeps-queue-and-original-id',[w,state({nativePhase:'dispatched'}),w,state({ownerAbsence:true}),w,state({ownerAbsence:false}),{action:'restart'},w,w,{action:'input'}]);
for(const patch of [{capacityEpoch:'foreign'},{readThreadId:'foreign'},{readCwd:'/elsewhere'},{readEphemeral:true}])add('refused-'+Object.keys(patch)[0],[state(patch),w,{action:'input'}]);
add('unavailable-owner-before-content',[state({node:{connected:false}}),head,w]);
add('only-current-head-delivers',[{action:'head',input:'tail'},head]);
add('drain-stops-before-work',[{action:'drain',continueSteps:0},status,head]);
add('drain-time-slice-retains-runnable',[{action:'drain',millisPerRead:50},status,{action:'drain',continueSteps:1},status]);
add('active-turn-stops-polling-drain',[{action:'drain'},status]);
add('native-refusal-retains-original-failure',[w,state({nativePhase:'failed'}),w,status,{action:'input'}]);
for(const valid of [false,true])add('image-admission-'+valid,[{action:'image-send',valid},...many(w,valid?2:1),head],'completed-0.154.0');
add('cancel-before-native-dispatch',[{action:'cancel-first'},...many(w,3),status]);
add('cancel-attached-original-turn-before-invoke',[w,{action:'cancel-first'},...many(w,4),status]);
add('cancel-original-running-turn',[w,w,{action:'cancel-first'},...many(w,4),status]);
add('real-notice-input-delivery',[{action:'notify',caller:'producer'},w,w,status,{action:'diagnostic-notice'}],'completed-0.154.0');
add('real-diagnostic-main-readiness',[{action:'diagnostic-find'},{action:'diagnostic-ready'}],'completed-0.154.0');
add('extra-live-input-revision-before-dispatch',[w,state({bumpInputOnRead:true}),w,w,{action:'input'}]);
add('extra-lost-result-reply',[state({turnPhase:'completed',loseResultReply:true}),{action:'drain'},reopen,{action:'drain'},status,head]);
add('extra-real-diagnostic-notice-publication',[state({turnPhase:'completed'}),{action:'diagnostic-notify'},{action:'drain'},{action:'diagnostic-notice'},head],'completed-0.154.0');

for(const c of cases){const e=environment(seeds[c.seed],false,extension());c.expected=[];try{for(const s of c.steps)c.expected.push(await e.snapshot(await result(()=>e.run(s))));}finally{e.close();}}
for(const c of cases.filter(c=>/^(loaded-original-|unloaded-original-|terminal-worker-drain-|full-read-fallback-)/.test(c.label)))if(c.expected.some(s=>s.output.error))throw Error('Successful original delivery path failed '+c.label+JSON.stringify(c.expected.map(s=>s.output)));
for(const c of cases.filter(c=>/^(busy-|unloaded-after-|same-epoch-|resume-full-|cancel-|image-admission-|drain-|active-turn-|real-diagnostic-)/.test(c.label)))if(c.expected.some(s=>s.output.error))throw Error('Successful original composition failed '+c.label+JSON.stringify(c.expected.map(s=>s.output)));
for(const c of cases.filter(c=>c.label.startsWith('extra-')&&!c.label.includes('lost-result')))if(c.expected.some(s=>s.output.error))throw Error('Original guard/notice integration failed '+c.label+JSON.stringify(c.expected.map(s=>s.output)));
{
 const c=cases.find(c=>c.label==='extra-lost-result-reply'),faults=c.expected.flatMap(s=>s.transport).filter(t=>t.lostResultReply);
 if(faults.length!==1||faults[0].params.create.contractKey!=='chat-bridge/result'||faults[0].result.error?.code!=='outcome_unknown'||c.expected.at(-1).output.value!==null||c.expected.at(-1).records.filter(r=>r.object.contractKey==='chat-bridge/result').length!==2)throw Error('Original explicit Result fault was not recovered exactly');
}
process.stdout.write(JSON.stringify({now,instructions,noticeInstructions,seeds,cases,boundary:'Only synthetic owner/Hive replies and clock are controlled. Original Main, Delivery, Worker, Collector, Publisher and CancelWorker execute unchanged; no approval exception is exercised by these explicit-binding sends.'}));
