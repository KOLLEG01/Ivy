import {existsSync,readFileSync,readdirSync} from 'node:fs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {canonical,digest,hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {managementFrameBytes}=await import('../../packages/contracts/src/limits.ts');
const {validateAgent}=await import('../../packages/contracts/src/agent-validation.ts');
const {validateTaskBoard}=await import('../../packages/contracts/src/task-board-validation.ts');
const {checkedNativeContract:catalogFor}=await import('../../packages/contracts/src/checked-native-contract.ts');
const {validateNativeRead}=await import('../../packages/sdk/src/native-evidence.ts');
const {nativeTurnItems}=await import('../../packages/sdk/src/native-observations.ts');
const load=n=>readFileSync('services/task-board/src/runtime/'+n+'.ts','utf8').replace(/^import .*;\r?\n/gm,'').replaceAll('export ','');
const mutation=new Function('hashJson',stripTypeScriptTypes(load('store').match(/const mutation = [\s\S]*?;/)[0])+'\nreturn mutation;')(hashJson);
const bytes=new Function('canonical','digest','requireThat','IvyError','managementFrameBytes',stripTypeScriptTypes(load('evidence-bytes'))+'\nreturn {saveEvidenceChunks,readEvidenceChunks,evidenceMaximumBytes};')(canonical,digest,requireThat,IvyError,managementFrameBytes);
const Reads=new Function('canonical','digest','hashJson','requireThat','validateNativeRead','validateTaskBoard','catalogFor','evidenceMaximumBytes','saveEvidenceChunks','readEvidenceChunks','mutation',stripTypeScriptTypes(load('read-evidence'),{mode:'transform'})+'\nreturn TaskBoardReadEvidence;')(canonical,digest,hashJson,requireThat,validateNativeRead,validateTaskBoard,catalogFor,bytes.evidenceMaximumBytes,bytes.saveEvidenceChunks,bytes.readEvidenceChunks,mutation);
const same=(a,b)=>a===undefined||b===undefined?a===b:canonical(a)===canonical(b);
const nativeRecord=v=>v!==null&&typeof v==='object'&&!Array.isArray(v)?v:{},nativeArray=v=>Array.isArray(v)?v:[],nativeString=v=>typeof v==='string'?v:'';
const Turns=new Function('canonical','hashJson','requireThat','validateAgent','validateTaskBoard','nativeTurnItems','catalogFor','same','nativeArray','nativeRecord','nativeString','TaskBoardReadEvidence','evidenceMaximumBytes','mutation',stripTypeScriptTypes(load('turn-evidence'),{mode:'transform'})+'\nreturn TaskBoardTurnEvidence;')(canonical,hashJson,requireThat,validateAgent,validateTaskBoard,nativeTurnItems,catalogFor,same,nativeArray,nativeRecord,nativeString,Reads,bytes.evidenceMaximumBytes,mutation);
const {TaskBoardFullTurnEvidence:Full,fullTurnParams,firstItemsParams}=new Function('canonical','requireThat','validateTaskBoard','same','evidenceMaximumBytes','nativeArray','nativeRecord','nativeString','TaskBoardTurnEvidence',stripTypeScriptTypes(load('full-turn-evidence'),{mode:'transform'})+'\nreturn {TaskBoardFullTurnEvidence,fullTurnParams,firstItemsParams};')(canonical,requireThat,validateTaskBoard,same,bytes.evidenceMaximumBytes,nativeArray,nativeRecord,nativeString,Turns);
const id=n=>'00000000-0000-4000-8000-'+String(n).padStart(12,'0'),pin=(n,r=1)=>({objectId:id(n),revision:r}),time=n=>'2026-01-02T03:04:05.'+String(n).padStart(3,'0')+'Z';
const groups=[];
for(const directory of readdirSync('specs/native').filter(n=>/^codex-/.test(n))){
 const path='specs/native/'+directory+'/catalog.json';if(!existsSync(path))continue;const c=JSON.parse(readFileSync(path)),catalog=catalogFor(c.version,{sourceHash:c.sourceHash});
 const run={target:{serviceNodeId:'native-agent'},plan:{nativeVersion:c.version,catalogSourceHash:c.sourceHash},primaryResourceRef:{namespace:'codex',kind:'thread',serviceNodeId:'native-agent',nativeId:'thread'},turnId:'turn',nativeEpoch:'epoch',originalRequest:{original:true}};
 const item={id:'item',type:'agentMessage',phase:'final_answer',text:'answer'};let wrap=false;try{nativeTurnItems(catalog.contract,{data:[item]},'turn');}catch{wrap=true;}
 const rows=[],add=(name,scenario,input={})=>rows.push({name,scenario,input});
 add('snapshot completion and replay','snapshot');add('single assistant output','output');add('two-page transcript and published artifact','transcript');add('stored name and replay identity','label');add('commentary is not final output','output',{item:{phase:'commentary'}});add('native wrapper identity follows retained layout','output',{wrongWrappedTurn:true});
 add('full proof exact unsupported first page','full');add('full proof opaque shifted cursors','full',{cursor:'opaque',verificationCursor:'shifted'});
 add('full proof final answer concatenation','full',{items:[item,{...item,id:'second',text:'second'}]});
 add('full proof last assistant fallback','full',{items:[{...item,phase:'analysis'},{...item,id:'second',phase:'commentary',text:'last'}]});
 add('full proof terminal status fallback','full',{status:'failed',items:[]});
 if(c.version==='0.159.2'){
  add('full proof lone surrogate summary','full',{items:[{...item,text:'retained\ud800'}]});
  for(const [name,input]of Object.entries({successfulItems:{unsupportedResult:true},arbitraryError:{unsupportedCode:-32000},laterItemPage:{unsupportedParams:{cursor:'later'}},wrongItemTurn:{unsupportedParams:{turnId:'other'}},wrongItemLimit:{unsupportedParams:{limit:1}},wrongItemDirection:{unsupportedParams:{sortDirection:'desc'}},nonterminal:{status:'inProgress'},runRevision:{valuePatch:{run:pin(200,2)}},fullEpoch:{fullEpoch:'other'},verificationEpoch:{verificationEpoch:'other'},referenceEpoch:{valuePatch:{unsupportedItems:{epoch:'other'}}},fullError:{fullError:true},verificationError:{verificationError:true},emptyPage:{fullData:[]},multipleTurns:{fullData:[{id:'turn',status:'completed'},{id:'other',status:'completed'}]},wrongTurn:{turn:{id:'other'}},changedFullStatus:{turn:{status:'failed'}},changedFinalStatus:{verificationStatus:'failed'},partialView:{turn:{itemsView:'notLoaded'}},bytes:{valuePatch:{fullTurnBytes:1}},unsupportedBeforeSnapshot:{unsupportedTime:1},fullBeforeUnsupported:{fullTime:1},verificationBeforeFull:{verificationTime:1},duplicateSnapshotObservation:{unsupportedId:2},duplicateFullObservation:{verificationId:4},completionTime:{valuePatch:{createdAt:time(4)}},duplicateItem:{items:[item,item]},missingItemId:{items:[{type:'commandExecution'}]},emptyItemId:{items:[{id:'',type:'commandExecution'}]},unicode:{items:[{...item,text:'a'+'😄'.repeat(65536)}]},emptyFinalAnswers:{items:[{...item,text:''},{...item,id:'b',text:''}]},wrongArtifactHash:{artifactPatch:{contentHash:'sha256:'+'0'.repeat(64)}},wrongArtifactMedia:{artifactPatch:{mediaType:'text/plain'}},unknownField:{valuePatch:{extra:true}}}))add('full proof '+name,'full',input);
  add('historical request uses full management frame bound','large');
  add('failed turn completion','snapshot',{status:'failed'});add('interrupted turn completion','snapshot',{status:'interrupted'});
  add('page body byte count after aggregate admission','transcript',{pagePatch:{nativeBytes:1},totalFromPages:true});
  add('page body item count','transcript',{pagePatch:{itemCount:0}});
  add('page body next cursor','transcript',{pagePatch:{nextCursor:'other'}});
  add('opaque cursor cannot repeat','transcript',{repeatCursor:true});
  add('transcript UTF16 summary bound','transcript',{largeTranscript:true});
  for(const [name,input]of Object.entries({owner:{run:{primaryResourceRef:{...run.primaryResourceRef,serviceNodeId:'other'}}},turn:{turnData:[]},duplicate:{turnData:[{id:'turn',status:'completed'},{id:'turn',status:'completed'}]},ephemeral:{thread:{ephemeral:true}},thread:{thread:{id:'other'}},epoch:{turnEpoch:'other'},chronology:{metaTime:9},status:{status:'inProgress'},reply:{metadataError:true}}))add('snapshot '+name,'snapshot',input);
  add('snapshot history cannot substitute request','history');add('snapshot corrupt body refuses read','corrupt');add('snapshot artifact hash','artifact',{patch:{contentHash:'sha256:'+'0'.repeat(64)}});
  add('snapshot partial write replay','retry');
  for(const [name,input]of Object.entries({empty:{item:{text:''}},analysis:{item:{phase:'analysis'}},multiple:{many:true},earlier:{outputTime:1},epoch:{outputEpoch:'other'},nonterminal:{status:'inProgress'},unicode:{item:{text:'a'+'😄'.repeat(65536)}}}))add('output '+name,'output',input);
  for(const [name,input]of Object.entries({cycle:{pagePatch:{previous:'self'}},index:{pagePatch:{index:4}},cursor:{pagePatch:{cursor:'other'}},bytes:{pagePatch:{nativeBytes:1}},count:{transcriptPatch:{pageCount:3}},total:{transcriptPatch:{nativeBytes:1}},collection:{pagePatch:{collectionId:'other'}},items:{duplicateItem:true},observation:{duplicateObservation:true},chronology:{pageTime:1},verification:{verificationStatus:'failed'},verificationIdentity:{verificationDuplicate:true},exhaustion:{lastCursor:'more'},noAssistant:{noAssistant:true},finalAnswers:{twoAnswers:true},fallback:{noFinal:true},epoch:{verificationEpoch:'other'}}))add('transcript '+name,'transcript',input);
 }
 for(const row of rows){
  const i=row.input,docs=[],blobs=[],trace=[],actions=[],results=[],writeIds=[];let next=400,fault=null;
  const put=(key,p,value,name=id(200),parent='root')=>{const d={key,pin:p,value:structuredClone(value),name,parent};docs.push(d);return d;};
  const latest=(key,a)=>{const list=docs.filter(d=>d.key===key&&d.pin.objectId===(typeof a==='string'?a:a.objectId));const d=typeof a==='string'?list.at(-1):list.find(d=>d.pin.revision===a.revision);requireThat(list.length,'task_board_scope_mismatch','The local workflow record has another scope.');requireThat(d,'task_board_evidence_mismatch','The local workflow revision is unavailable.');return d;};
  const validate=(key,v)=>{if(key!=='task-board/run')validateTaskBoard({'task-board/native-read-evidence':'NativeReadEvidence','task-board/native-turn-snapshot':'NativeTurnSnapshot','task-board/native-result-page':'NativeResultPage','task-board/native-transcript':'NativeTranscript','task-board/native-full-turn-transcript':'NativeFullTurnTranscript'}[key],v);};
  const store={async read(key,address,primary=true){trace.push(['read',key,address,primary]);const d=latest(key,address);requireThat(!primary||d.parent==='root','task_board_scope_mismatch','The local workflow record has another scope.');validate(key,d.value);return{pin:d.pin,value:d.value,metadata:{name:d.name,parentId:d.parent},revision:{contentHash:hashJson(d.value)}};},
   async write(key,value,m,d){trace.push(['write',key,value,m,d]);const old=docs.find(x=>x.key===key&&x.name===d.create.name&&x.parent===d.create.parentId);writeIds.push(old?.pin.objectId??id(next));validate(key,value);if(fault===key){fault=null;throw new IvyError('fixture_failed','Fixture write unavailable.');}if(old){requireThat(hashJson(old.value)===hashJson(value),'revision_conflict','The local TaskBoard identity differs.');return old.pin;}return put(key,pin(next++),value,d.create.name,d.create.parentId).pin;},
   saveLocalBlob(key,name,parent,body){trace.push(['blob-save',key,name,parent,digest(body),body.length]);let b=blobs.find(x=>x.key===key&&x.name===name&&x.parent===parent);if(b){requireThat(b.body.equals(body),'revision_conflict','The local evidence identity differs.');}else{b={key,name,parent,pin:pin(900+blobs.length),body:Buffer.from(body),initial:Buffer.from(body)};blobs.push(b);}return b.pin;},
   localBlob(key,p,parent){trace.push(['blob-read',key,p,parent]);const b=blobs.find(b=>b.key===key&&b.pin.objectId===p.objectId&&b.parent===parent);requireThat(b,'task_board_evidence_mismatch','The local evidence blob is unavailable.');return b.body;},};
  const active={...run,...i.run};put('task-board/run',pin(200,1),active);put('task-board/run',pin(200,2),active);
  const observed=(n,method,params,result,epoch='epoch',at=n)=>({schemaVersion:1,observationId:'read-'+n,callerPrincipalId:'caller',serviceNodeId:'native-agent',nativeVersion:c.version,nativeExecutableHash:c.nativeExecutableHash,catalogHash:catalog.catalogHash,epoch,requestId:n,observedAt:time(at),method,params,requestHash:hashJson({method,params}),reply:{result}});
  const metadata=observed(1,'thread/read',{threadId:'thread',includeTurns:false},{thread:{id:'thread',ephemeral:false,...i.thread}},'epoch',i.metaTime??1);if(i.metadataError)metadata.reply={error:{code:-32601,message:'unsupported'}};
  const turns=observed(2,'thread/turns/list',{threadId:'thread',cursor:null,limit:100,itemsView:'notLoaded',sortDirection:'desc'},{data:i.turnData??[{id:'turn',status:i.status??'completed'}]},i.turnEpoch??'epoch');
  const turnSubject=new Turns(store,'caller'),fullSubject=new Full(store,'caller');
  const shape=(v)=>v?.document?{value:v.document.value,...(v.summary!==undefined?{summary:v.summary,status:v.status}:{thread:v.thread,turn:v.turn,run:v.source.run.value})}:v?.snapshot?{snapshot:v.snapshot.document.value,status:v.status,summary:v.summary}:v;
  const action=async(method,args)=>{actions.push({method,args:structuredClone(args)});try{let v;if(method==='patch'){const d=latest(args[0],args[1]);Object.assign(d.value,args[2]);if(args[3])d.name=args[3];v=null;}else if(method==='largeRequest'){for(const d of docs.filter(d=>d.key==='task-board/run'))d.value.originalRequest={body:'x'.repeat(args[0])};v=null;}else if(method==='corrupt'){blobs[0].body[0]^=1;v=null;}else if(method==='fault'){fault=args[0];v=null;}else if(method==='write'){v=await store.write(...args);}else if(method==='saveRead'){const o=args[1];v=await new Reads(store).save(args[0],o,{observationId:o.observationId,callerPrincipalId:'caller',serviceNodeId:'native-agent',nativeVersion:c.version,epoch:o.epoch,method:o.method,params:o.params});}else if(method==='verifyFull')v=await fullSubject.verify(...args);else if(method==='fullTranscript')v=await fullSubject.transcript(...args);else v=await turnSubject[method](...args);results.push({ok:shape(v)});return v;}catch(e){if(!(e instanceof IvyError))throw new Error(row.name,{cause:e});results.push({error:{code:e.code,message:e.message,outcome:e.outcome}});return undefined;}};
  if(row.scenario==='large')await action('largeRequest',[17*1024*1024]);
  const snapshot=await action('saveSnapshot',[pin(200),metadata,turns]);
  if(!snapshot){if(row.scenario==='retry')throw Error('Unexpected setup failure');}
  else if(row.scenario==='snapshot'){await action('snapshot',[pin(200,2),snapshot]);await action('completion',[pin(200,2),snapshot,null]);await action('saveSnapshot',[pin(200),metadata,turns]);}
  else if(row.scenario==='label'){await action('patch',['task-board/native-turn-snapshot',snapshot.object,{},'Retained original name']);await action('saveSnapshot',[pin(200),metadata,turns]);}
  else if(row.scenario==='history'){await action('patch',['task-board/run',pin(200,2),{originalRequest:{other:true}}]);await action('snapshot',[pin(200,2),snapshot]);}
  else if(row.scenario==='corrupt'){await action('corrupt',[]);await action('snapshot',[pin(200),snapshot]);}
  else if(row.scenario==='artifact')await action('snapshot',[pin(200),{...snapshot,...i.patch}]);
  else if(row.scenario==='large')await action('completion',[pin(200,2),snapshot,null]);
  else if(row.scenario==='retry'){await action('fault',['task-board/native-turn-snapshot']);await action('saveSnapshot',[pin(200),metadata,observed(3,'thread/turns/list',turns.params,turns.reply.result)]);await action('saveSnapshot',[pin(200),metadata,observed(3,'thread/turns/list',turns.params,turns.reply.result)]);}
  else if(row.scenario==='output'){
   const value={...item,...i.item};const entry=v=>wrap?{turnId:i.wrongWrappedTurn?'other':'turn',item:v}:v;
   const data=i.many?[entry(value),entry({...value,id:'second'})]:[entry(value)];
   const output=observed(3,'thread/items/list',{threadId:'thread',turnId:'turn',cursor:null,limit:1,sortDirection:'desc'},{data},i.outputEpoch??'epoch',i.outputTime??3);
   await action('saveOutput',[pin(200),snapshot,null,output]);
  }else if(row.scenario==='full'){
   const unsupported=observed(i.unsupportedId??3,'thread/items/list',{...firstItemsParams('thread','turn'),...i.unsupportedParams},{data:[]},'epoch',i.unsupportedTime??3);
   if(!i.unsupportedResult)unsupported.reply={error:{code:i.unsupportedCode??-32601,message:'unsupported'}};
   const full=observed(4,'thread/turns/list',fullTurnParams('thread',i.cursor??null,true),{data:i.fullData??[{id:'turn',status:i.status??'completed',itemsView:'full',items:i.items??[item],...i.turn}]},i.fullEpoch??'epoch',i.fullTime??4);
   const verification=observed(i.verificationId??5,'thread/turns/list',fullTurnParams('thread',i.verificationCursor??i.cursor??null,false),{data:[{id:'turn',status:i.verificationStatus??i.status??'completed'}]},i.verificationEpoch??'epoch',i.verificationTime??5);
   if(i.fullError)full.reply={error:{code:-32601,message:'unsupported'}};if(i.verificationError)verification.reply={error:{code:-32601,message:'unsupported'}};
   const ref=async o=>({artifact:await action('saveRead',[pin(200),o]),observationId:o.observationId,epoch:o.epoch});
   const value={schemaVersion:1,collectionId:'collection',run:pin(200),snapshot,unsupportedItems:await ref(unsupported),fullTurn:await ref(full),fullTurnCursor:i.cursor??null,fullTurnBytes:Buffer.byteLength(canonical(full)),verification:await ref(verification),verificationCursor:i.verificationCursor??i.cursor??null,createdAt:verification.observedAt};
   for(const[k,v]of Object.entries(i.valuePatch??{}))value[k]=k==='unsupportedItems'?{...value[k],...v}:v;
   await action('verifyFull',[pin(200,2),value]);
   const p=await action('write',['task-board/native-full-turn-transcript',value,'fixture-full-transcript',{create:{name:'Full transcript',parentId:id(200)}}]);
   if(p)await action('fullTranscript',[pin(200,2),{object:p,label:'Full transcript',contentHash:hashJson(value),mediaType:'application/json',...i.artifactPatch}]);
  }else if(row.scenario==='transcript'){
   const snap=latest('task-board/native-turn-snapshot',snapshot.object).value;
   const entry=v=>wrap?{turnId:'turn',item:v}:v;
   const first={...item,id:'a',phase:i.noFinal?'analysis':'final_answer',text:i.largeTranscript?'a'+'😄'.repeat(65536):i.twoAnswers?'first':'answer'};if(i.noAssistant)first.type='commandExecution';
   const second={...item,id:i.duplicateItem?'a':'b',phase:i.noFinal?'analysis':i.twoAnswers?'final_answer':'commentary',text:'last'};if(i.noAssistant)second.type='commandExecution';
   const one=observed(3,'thread/items/list',{threadId:'thread',turnId:'turn',cursor:null,limit:100,sortDirection:'asc'},{data:[entry(first)],nextCursor:'next'},'epoch',i.pageTime??3);
   const two=observed(i.duplicateObservation?3:4,'thread/items/list',{threadId:'thread',turnId:'turn',cursor:'next',limit:100,sortDirection:'asc'},{data:[entry(second)],nextCursor:i.repeatCursor?'next':i.lastCursor??null});
   const p1=await action('savePage',[pin(200),snap,'collection',null,1,null,one]);
   let p2=await action('savePage',[pin(200),snap,'collection',p1,2,'next',two]);
   let third;if(i.repeatCursor){third=observed(5,'thread/items/list',two.params,{data:[entry({...item,id:'c'})],nextCursor:null});p2=await action('savePage',[pin(200),snap,'collection',p2,3,'next',third]);}
   if(i.pagePatch){const patch={...i.pagePatch};if(patch.previous==='self')patch.previous=p2;await action('patch',['task-board/native-result-page',p2,patch]);}
   const verification=observed(i.verificationDuplicate?4:i.repeatCursor?6:5,'thread/turns/list',turns.params,{data:[{id:'turn',status:i.verificationStatus??'completed'}]},i.verificationEpoch??'epoch',i.repeatCursor?6:5);
   const evidence=await new Reads(store).save(pin(200),verification,{observationId:verification.observationId,callerPrincipalId:'caller',serviceNodeId:'native-agent',nativeVersion:c.version,epoch:verification.epoch,method:verification.method,params:verification.params});
   // This explicit setup call also executes through the Rust reads action.
   actions.push({method:'saveRead',args:[pin(200),verification]});results.push({ok:evidence});
   const transcript={schemaVersion:1,collectionId:'collection',run:pin(200),snapshot,phase:'complete',head:p2,pageCount:i.repeatCursor?3:2,nativeBytes:i.totalFromPages?docs.filter(d=>d.key==='task-board/native-result-page').reduce((n,p)=>n+p.value.nativeBytes,0):Buffer.byteLength(canonical(one))+Buffer.byteLength(canonical(two))+(third?Buffer.byteLength(canonical(third)):0),nextCursor:null,verification:{artifact:evidence,observationId:verification.observationId,epoch:verification.epoch},verificationCursor:null,createdAt:time(2),updatedAt:time(5),...i.transcriptPatch};
   await action('verifyTranscript',[pin(200,2),transcript]);
   const p=await action('write',['task-board/native-transcript',transcript,'fixture-transcript',{create:{name:'Transcript',parentId:id(200)}}]);
   if(p)await action('transcript',[pin(200,2),{object:p,label:'Transcript',contentHash:hashJson(transcript),mediaType:'application/json'}]);
  }
  const documents=docs.filter((d,index)=>d.key!=='task-board/run'&&!docs.slice(index+1).some(x=>x.pin.objectId===d.pin.objectId)).map(d=>({key:d.key,pin:d.pin,value:d.value,name:d.name,parent:d.parent})).sort((a,b)=>a.pin.objectId<b.pin.objectId?-1:1);
  row.actions=actions;row.run=active;row.writeIds=writeIds;row.expectedHash=hashJson({results,trace,documents});row.blobs=blobs.map(b=>({key:b.key,name:b.name,parent:b.parent,pin:b.pin,hex:b.initial.toString('hex')}));
  if(row.scenario==='large'||['single assistant output','two-page transcript and published artifact','stored name and replay identity','full proof exact unsupported first page','full proof opaque shifted cursors','full proof final answer concatenation','full proof last assistant fallback','full proof terminal status fallback'].includes(row.name))assert.ok(results.every(r=>'ok'in r),row.name+JSON.stringify(results));
 }
 groups.push({path,rows});
}
process.stdout.write(JSON.stringify(groups));
