import './chat-domain-loader.mjs';
const {ChatCollectionReads}=await import('../../../services/chat-bridge/src/collection-reads.ts');
const {checkedNativeContract}=await import('../../../packages/contracts/src/checked-native-contract.ts');
const {nativeVersions}=await import('../../../packages/contracts/src/native-versions.ts');
const {nativeTurnItems}=await import('../../../packages/sdk/src/native-observations.ts');
const {canonical,hashJson,digest}=await import('../../../packages/contracts/src/canonical.ts');
const {IvyError}=await import('../../../packages/contracts/src/errors.ts');
const outcome=fn=>{try{return{value:fn()};}catch(error){const e=IvyError.from(error);return{error:{code:e.code,message:e.message,outcome:e.outcome}};}};
const catalogs=nativeVersions.map(version=>{
 const selected=checkedNativeContract(version),probe=outcome(()=>nativeTurnItems(selected.contract,{data:[{turnId:'original',item:{id:'wrapped',type:'agentMessage',text:'wrapped'},id:'direct',type:'agentMessage',text:'direct'}]},'original'));
 return{version,catalogHash:selected.catalogHash,nativeExecutableHash:selected.catalog.nativeExecutableHash,layout:probe.value?(probe.value[0].id==='wrapped'?'wrapped':'direct'):'unsupported',probeError:probe.error??null};
});
const message=(id,text=id,extra={})=>({id,type:'agentMessage',text,...extra});
const turn=(id='original',status='completed',extra={})=>({id,status,items:[],itemsView:'notLoaded',...extra});
const turns=(data,nextCursor=null,extra={})=>({action:'append',reply:{result:{data,nextCursor}},...extra});
const items=(data,nextCursor=null,extra={})=>({...turns(data,nextCursor),items:true,...extra});
const unsupported={action:'append',reply:{error:{code:-32601,message:'Native items unsupported.'}}};
const start=()=>turns([turn()]),finish=()=>turns([turn()]);
const repeat=(unit,count)=>({$repeatText:{unit,count}});
const cases=[];
const add=(label,steps,extra={})=>cases.push({label,version:'0.154.0',kind:'behavior',threadId:'main',turnId:'original',steps,...extra});
add('search-items-fork-and-backward-clock',[
 turns([turn('manual')],'older'),turns([turn('original','inProgress')]),start(),
 items([message('a','A😀'),message('b',''),message('comment','hidden',{phase:'commentary'}),{id:'tool',type:'commandExecution'}],'next'),
 {action:'fork',from:'main',target:'branch'},items([message('c','Complete')],null,{state:'branch'}),
 turns([turn()],null,{state:'branch',at:'2026-09-07T02:00:00.000Z'}),{action:'next',state:'branch'},
 items([message('c','Separate')]),finish(),
]);
add('full-fallback-keeps-found-cursor',[turns([turn('manual')],'older'),start(),unsupported,
 turns([turn('original','completed',{itemsView:'full',items:[message('answer','Original\n😀text')]})]),
 turns([turn('manual-later')],'verify-page'),finish(),{action:'next'}]);
for(const status of ['failed','interrupted'])add('terminal-'+status,[turns([turn('original',status)]),items([]),turns([turn('original',status)])]);
for(const c of catalogs){
 add('catalog-item-layout-'+c.version,[start(),items([message('item','Native '+c.version)]),...(c.layout==='unsupported'?[]:[finish()])],{version:c.version});
 add('catalog-full-fallback-'+c.version,[start(),unsupported,turns([turn('original','completed',{itemsView:'full',items:[message('item','Full '+c.version)]})]),finish()],{version:c.version});
}
add('wrong-method-before-read-identity',[{...start(),patch:{method:'thread/read'}}]);
add('changed-query-before-read-identity',[{...start(),patch:{params:{threadId:'main',cursor:null,limit:2,itemsView:'notLoaded',sortDirection:'desc'}}}]);
add('duplicate-observation-with-correct-next-query',[{...start(),id:'same'},items([]),turns([turn()],null,{id:'same'})]);
add('old-query-is-not-current',[start(),{action:'append',reuse:0}]);
add('search-native-error',[unsupported]);
for(const code of [-32000,'-32601'])add('unsupported-error-code-'+code,[start(),{action:'append',reply:{error:{code,message:'Owner read failed.'}}}]);
add('late-unsupported-items',[start(),items([message('first')],'second'),unsupported]);
add('unsupported-after-failed-item-is-not-first',[start(),items([message('bad','\ud800')]),unsupported]);
add('empty-nonfinal-items',[start(),items([],'gap')]);
add('empty-final-items',[start(),items([]),finish()]);
add('omitted-final-cursor',[start(),{action:'append',items:true,reply:{result:{data:[]}}},finish()]);
add('missing-original-turn',[turns([])]);
add('nonfinal-empty-turn-page',[turns([],'later')]);
add('more-than-one-turn',[turns([turn('one'),turn('two')],'later')]);
add('missing-data-array',[{action:'append',reply:{result:{nextCursor:null}}}]);
add('nonarray-data',[{action:'append',reply:{result:{data:{},nextCursor:null}}}]);
for(const cursor of ['',0,false,[],{}]){
 add('invalid-search-cursor-'+JSON.stringify(cursor),[turns([turn('other')],cursor)]);
 add('invalid-items-cursor-'+JSON.stringify(cursor),[start(),items([message('valid')],cursor)]);
}
add('cursor-validation-before-data-array',[{action:'append',reply:{result:{data:null,nextCursor:''}}}]);
add('search-cursor-loop',[turns([turn('a')],'again'),turns([turn('b')],'again'),turns([turn('b')],'fresh')]);
add('search-cursor-cycle',[turns([turn('a')],'one'),turns([turn('b')],'two'),turns([turn('c')],'one')]);
add('search-repeated-turn',[turns([turn('same')],'one'),turns([turn('same')],'two')]);
add('search-empty-foreign-turn-id',[turns([turn('')],'older'),start(),items([]),finish()]);
add('items-cursor-cycle',[start(),items([message('a')],'one'),items([message('b')],'two'),items([message('c')],'one'),items([message('c')],'three')]);
add('duplicate-item-in-page',[start(),items([message('same','first'),message('same','second')]),items([message('same','retry')])]);
add('duplicate-item-across-pages',[start(),items([message('same')],'later'),items([message('same')])]);
for(const text of ['\ud800','\udfff',4,null])add('lossy-or-nonstring-text-'+JSON.stringify(text),[start(),items([message('bad',text)]),items([message('bad','fixed')])]);
for(const id of ['',4,null])add('invalid-item-identity-'+JSON.stringify(id),[start(),items([message(id,'text')])]);
add('commentary-and-tools-retain-identity',[start(),items([message('comment','ignored',{phase:'commentary'}),{id:'tool',type:'commandExecution',text:4},message('answer','😀\n\u0000é')],'later'),items([message('comment','still duplicate')])]);
add('foreign-wrapped-turn',[start(),items([message('foreign')],null,{itemTurn:'foreign'})]);
for(const [label,body]of [
 ['shifted',turn('other','completed',{itemsView:'full'})],
 ['status',turn('original','failed',{itemsView:'full'})],
 ['partial',turn()],['missing-items',turn('original','completed',{itemsView:'full',items:null})],
])add('full-page-'+label,[start(),unsupported,turns([body])]);
add('full-page-two-turns',[start(),unsupported,turns([turn(),turn()])]);
for(const status of ['failed','inProgress','unknown'])add('changed-verification-'+status,[start(),items([message('answer')]),turns([turn('original',status)])]);
add('verification-follows-original-search',[start(),items([message('answer')]),turns([turn('newer')],'older'),finish()]);
add('unpaired-native-identities-and-cursor',[
 turns([turn('other')],'\ud800'),turns([turn('original\udfff')]),items([message('id\ud800','Reply')]),turns([turn('original\udfff')]),
],{threadId:'main\ud800',turnId:'original\udfff'});
add('fork-keeps-private-observation-and-item-membership',[start(),items([message('same')],'later'),{action:'fork',from:'main',target:'branch'},items([message('same')],null,{state:'branch'}),items([message('new')]),finish()]);
add('in-progress-not-retained',[turns([turn('original','inProgress')],null,{id:'not-retained'}),turns([turn('original','inProgress')],null,{id:'not-retained'}),start(),items([]),finish()]);
add('observed-times-do-not-order-pages',[turns([turn('manual')],'older',{at:'2026-09-07T04:00:00.000Z'}),turns([turn()],null,{at:'2026-09-07T03:00:00.000Z'}),items([message('answer')],null,{at:'2026-09-07T02:00:00.000Z'}),turns([turn()],null,{at:'2026-09-07T01:00:00.000Z'})]);
add('actual-1024-read-bound',[...Array.from({length:1024},(_,i)=>turns([turn('other-'+i)],'cursor-'+i)),start()],{kind:'limits'});
add('actual-64-mib-evidence-bound',[start(),...Array.from({length:10},(_,i)=>items([message('large-'+i,repeat('x',7*1024*1024))],'cursor-'+i))],{kind:'limits'});
add('single-observation-16-mib-bound',[start(),items([message('oversize',repeat('x',16*1024*1024))])],{kind:'limits'});

const expand=value=>{
 if(Array.isArray(value))return value.map(expand);
 if(value&&typeof value==='object'){
  if(value.$repeatText)return value.$repeatText.unit.repeat(value.$repeatText.count);
  return Object.fromEntries(Object.entries(value).map(([key,value])=>[key,expand(value)]));
 }
 return value;
};
const textSummary=text=>({utf16Length:text.length,byteLength:Buffer.byteLength(text),contentHash:digest(Buffer.from(text)),...(text.length<128?{value:text}:{})});
const snapshot=states=>Object.fromEntries([...states].map(([key,state])=>[key,{
 phase:state.phase,cursor:state.cursor,foundCursor:state.foundCursor,nativeState:state.nativeState,nativeBytes:state.nativeBytes,count:state.count,textBytes:state.textBytes,lastObservedAt:state.lastObservedAt,texts:state.texts.map(textSummary),next:outcome(()=>state.next()),
}]));
for(const c of cases){
 const selected=catalogs.find(v=>v.version===c.version),states=new Map([['main',new ChatCollectionReads(c.threadId,c.turnId)]]),observations=[];
 c.expected=[];
 for(const [index,step]of c.steps.entries()){
  let observed=null;
  const output=outcome(()=>{
   const state=states.get(step.state??'main');
   if(step.action==='next')return state.next();
   if(step.action==='fork'){states.set(step.target,states.get(step.from).fork());return null;}
   if(step.action!=='append')throw Error('Unknown reducer fixture action.');
   if(step.reuse!==undefined)observed=structuredClone(observations[step.reuse]);
   else{
    const request=state.next(),reply=expand(step.reply);
    if(step.items&&selected.layout==='wrapped')reply.result.data=reply.result.data.map(item=>({turnId:step.itemTurn??state.turnId,item}));
    observed={schemaVersion:1,observationId:step.id??c.label+':observation-'+index,requestId:c.label+':request-'+index,serviceNodeId:'agent',callerPrincipalId:'chat',epoch:'epoch',nativeVersion:c.version,nativeExecutableHash:selected.nativeExecutableHash,catalogHash:selected.catalogHash,...request,requestHash:hashJson(request),observedAt:step.at??'2026-09-07T04:00:00.000Z',reply,...structuredClone(step.patch??{})};
   }
   observations.push(observed);
   return state.append(observed);
  });
  const encoded=observed?canonical(observed,32*1024*1024):null;
  c.expected.push({output,observation:encoded===null?null:{byteLength:Buffer.byteLength(encoded),contentHash:digest(Buffer.from(encoded))},states:snapshot(states)});
 }
 if(c.label==='actual-1024-read-bound'&&(c.expected.at(-1).output.error?.code!=='chat_collection_limit'||states.get('main').count!==1024))throw Error('Read count bound was not exercised.');
 if(c.label==='actual-64-mib-evidence-bound'&&(c.expected.at(-1).output.error?.code!=='chat_collection_limit'||states.get('main').texts.length!==9))throw Error('Actual evidence bound was not exercised.');
 if(c.label==='in-progress-not-retained'&&(c.expected[0].output.value!==false||c.expected[1].states.main.count!==0))throw Error('In-progress observations were unexpectedly retained.');
}
process.stdout.write(JSON.stringify({catalogs,cases}));
