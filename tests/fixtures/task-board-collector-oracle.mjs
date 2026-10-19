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
const {serviceTools}=await import('../../packages/sdk/src/client.ts');
const {validateNativeStatus}=await import('../../packages/sdk/src/native-evidence.ts');
const turnSource=load('turn-evidence'),helpers=turnSource.slice(turnSource.indexOf('const pageCursor'),turnSource.indexOf('/** Verifies read artifacts'));
const {pageCursor,turnsParams}=new Function('requireThat',stripTypeScriptTypes(helpers)+'\nreturn {pageCursor,turnsParams};')(requireThat);
const time=n=>'2026-01-02T03:04:05.'+String(n).padStart(3,'0')+'Z';
class ClockDate extends Date{constructor(){super(time(6));}}
const Inspector=new Function('hashJson','requireThat','validateAgent','serviceTools','validateNativeStatus','same','catalogFor','nativeArray','nativeRecord','TaskBoardReadEvidence','mutation','pageCursor','turnsParams','Date',stripTypeScriptTypes(load('native-inspector'),{mode:'transform'})+'\nreturn TaskBoardNativeInspector;')(hashJson,requireThat,validateAgent,serviceTools,validateNativeStatus,same,catalogFor,nativeArray,nativeRecord,Reads,mutation,pageCursor,turnsParams,ClockDate);
const FullCollector=new Function('canonical','IvyError','requireThat','evidenceMaximumBytes','firstItemsParams','fullTurnParams','TaskBoardFullTurnEvidence','nativeRecord','mutation',stripTypeScriptTypes(load('native-full-turn-collector'),{mode:'transform'})+'\nreturn TaskBoardFullTurnCollector;')(canonical,IvyError,requireThat,bytes.evidenceMaximumBytes,firstItemsParams,fullTurnParams,Full,nativeRecord,mutation);
const Collector=new Function('hashJson','IvyError','requireThat','TaskBoardNativeInspector','mutation','TaskBoardTurnEvidence','nativeRecord','TaskBoardFullTurnCollector',stripTypeScriptTypes(load('native-collector'),{mode:'transform'})+'\nreturn TaskBoardNativeCollector;')(hashJson,IvyError,requireThat,Inspector,mutation,Turns,nativeRecord,FullCollector);
const id=n=>'00000000-0000-4000-8000-'+String(n).padStart(12,'0'),pin=(n,r=1)=>({objectId:id(n),revision:r});
const item={id:'a',type:'agentMessage',phase:'final_answer',text:'answer'},comment={...item,id:'b',phase:'commentary',text:'last'};
const page=(data=[],nextCursor=null)=>({reply:{result:{data,nextCursor}}});
const found=()=>page([{id:'turn',status:'completed'}]),meta=()=>({reply:{result:{thread:{id:'thread',ephemeral:false}}}}),unsupported=()=>({reply:{error:{code:-32601,message:'unsupported'}}});
const full=()=>page([{id:'turn',status:'completed',itemsView:'full',items:[item]}]);
const groups=[];
for(const directory of readdirSync('specs/native').filter(n=>/^codex-/.test(n))){
 const path='specs/native/'+directory+'/catalog.json';if(!existsSync(path))continue;
 const c=JSON.parse(readFileSync(path)),catalog=catalogFor(c.version,{sourceHash:c.sourceHash});
 let wrap=false;try{nativeTurnItems(catalog.contract,{data:[item]},'turn');}catch{wrap=true;}
 const entry=v=>wrap?{turnId:'turn',item:v}:v;
 const two=[page([entry(item)],'next'),page([entry(comment)]),found(),meta(),found()];
 const fallback=[unsupported(),found(),meta(),found(),full(),found()];
 const rows=[],add=(name,input={})=>rows.push({name,input:{steps:two,...input}});
 add('paged completion and idempotent replay',{replay:true});add('bounded page checkpoint resumes',{budget:1,passes:2,replay:true});
 add('exact first unsupported item falls back',{steps:fallback,replay:true});
 add('full fallback resumes bounded turn search',{budget:1,passes:2,steps:[unsupported(),page([],'next'),unsupported(),found(),meta(),found(),full(),found()],replay:true});
 if(c.version==='0.159.2'){
  add('arbitrary native error has no fallback',{steps:[{reply:{error:{code:-32000,message:'failed'}}}]});
  add('later unsupported item has no fallback',{steps:[two[0],unsupported()]});
  add('repeated cursor retains immutable page before refusal',{steps:[two[0],page([entry(comment)],'next')]});
  add('final native status changed',{steps:[two[0],two[1],found(),meta(),page([{id:'turn',status:'failed'}])]});
  add('final turn unavailable',{steps:[two[0],two[1],page()]});
  add('final turn search resumes without rereading exhausted items',{budget:1,passes:2,steps:[page([entry(item)]),page([],'next'),found(),meta(),found()],replay:true});
  add('owner lost before begin',{failOwner:1});add('owner lost before native collection read',{failOwner:4});
  add('checkpoint failure resumes from durable prefix',{fault:'task-board/native-transcript',passes:2,steps:[two[0],...two],replay:true});
  add('begin competing stable identity',{race:'begin',replay:true});add('full competing stable identity',{race:'full',steps:fallback,replay:true});
  add('full native error retains original receipts',{steps:[...fallback.slice(0,4),unsupported()]});
  add('full verification shifted turn',{steps:[...fallback.slice(0,5),page([{id:'other',status:'completed'}])]});
  add('full incomplete native view',{steps:[...fallback.slice(0,4),page([{id:'turn',status:'completed',itemsView:'notLoaded',items:[item]}]),found()]});
  add('collection rejects replacement epoch',{statusEpoch:'replacement'});
  for(const budget of [0,65,1.5])add('collection budget '+budget,{budget});
  add('collection rejects future run revision',{collectionPatch:{run:pin(200,3)}});
  add('collection rejects preexisting verification',{collectionPatch:{verification:{artifact:{object:pin(999),label:'receipt',contentHash:'sha256:'+'0'.repeat(64),mediaType:'application/json'},observationId:'other',epoch:'epoch'}}});
  for(const[name,patch]of Object.entries({index:{index:3},cursor:{cursor:'other'},bytes:{nativeBytes:1},epoch:{evidence:{epoch:'other'}},cycle:{previous:'self'}}))add('checkpoint refuses '+name,{budget:1,passes:2,pagePatch:patch});
  for(const[name,patch]of Object.entries({phase:{phase:'complete'},pages:{pageCount:1},head:{head:pin(999)},cursor:{nextCursor:'later'}}))add('direct fallback refuses '+name,{directFull:true,collectionPatch:patch,steps:[]});
 }
 for(const row of rows){
  const i=row.input,docs=[],blobs=[],trace=[],writeIds=[],actions=[],results=[],observations=[],statusReplies=[];
  let next=400,readCount=0,owners=0,fault=null,race=false;
  const put=(key,p,value,name=id(200),parent='root')=>{const d={key,pin:p,value:structuredClone(value),name,parent};docs.push(d);return d;};
  const latest=(key,a)=>{const list=docs.filter(d=>d.key===key&&d.pin.objectId===(typeof a==='string'?a:a.objectId)),d=typeof a==='string'?list.at(-1):list.find(d=>d.pin.revision===a.revision);requireThat(list.length,'task_board_scope_mismatch','The local workflow record has another scope.');requireThat(d,'task_board_evidence_mismatch','The local workflow revision is unavailable.');return d;};
  const validate=(key,value)=>{if(key!=='task-board/run')validateTaskBoard({'task-board/native-read-evidence':'NativeReadEvidence','task-board/native-turn-snapshot':'NativeTurnSnapshot','task-board/native-result-page':'NativeResultPage','task-board/native-transcript':'NativeTranscript','task-board/native-full-turn-transcript':'NativeFullTurnTranscript','task-board/native-turn-search':'NativeTurnSearch'}[key],value);};
  const document=d=>({pin:d.pin,value:structuredClone(d.value),metadata:{name:d.name,parentId:d.parent},revision:{contentHash:hashJson(d.value)}});
  const run={target:{serviceNodeId:'native-agent',hostId:'fixture'},plan:{nativeVersion:c.version,catalogSourceHash:c.sourceHash},primaryResourceRef:{namespace:'codex',kind:'thread',serviceNodeId:'native-agent',nativeId:'thread'},turnId:'turn',nativeEpoch:'epoch',originalRequest:{original:true}};
  put('task-board/run',pin(200),run);put('task-board/run',pin(200,2),run);
  const status={serviceNodeId:'native-agent',hostId:'fixture',nativeVersion:c.version,nativeExecutableHash:c.nativeExecutableHash,catalogHash:catalog.catalogHash,epoch:i.statusEpoch??'epoch',pid:42,state:'ready',observedAt:time(6),code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]};
  const observed=(n,method,params,reply)=>({schemaVersion:1,observationId:'read-'+n,callerPrincipalId:'caller',serviceNodeId:'native-agent',nativeVersion:c.version,nativeExecutableHash:c.nativeExecutableHash,catalogHash:catalog.catalogHash,epoch:'epoch',requestId:n,observedAt:time(n),method,params,requestHash:hashJson({method,params}),reply});
  const client={async request(method,params){trace.push(['rpc',method,structuredClone(params)]);if(method==='tools.list')return{provider:{node:{serviceNodeId:'native-agent'}},items:[{qualifiedName:params.namespace+'.'+params.namePrefix,definition:{namespace:params.namespace,name:params.namePrefix,interfaceVersion:'1.0.0',annotations:{readOnlyHint:true},inputSchema:{},outputSchema:{}},definitionHash:'sha256:'+'a'.repeat(64),catalogHash:'sha256:'+'b'.repeat(64)}],nextCursor:null};
   if(params.qualifiedName==='agent.status'){statusReplies.push(status);return status;}
   const step=i.steps[readCount++];if(!step)throw Error(row.name+': missing read '+readCount);
   const o={...observed(readCount+2,params.arguments.method,params.arguments.params,step.reply),...step};observations.push(o);return o;
  }};
  const store={client,
   async read(key,address,primary=true){trace.push(['read',key,address,primary]);const d=latest(key,address);requireThat(!primary||d.parent==='root','task_board_scope_mismatch','The local workflow record has another scope.');validate(key,d.value);return document(d);},
   async named(key,name,parent){trace.push(['named',key,name,parent]);const d=docs.filter(d=>d.key===key&&d.name===name&&d.parent===parent).at(-1);if(d)validate(key,d.value);return d?document(d):null;},
   async write(key,value,m,d){trace.push(['write',key,value,m,d]);const old=d.create?docs.filter(x=>x.key===key&&x.name===d.create.name&&x.parent===d.create.parentId).at(-1):latest(key,d.objectId);writeIds.push(old?.pin.objectId??id(next));validate(key,value);
    if(fault===key){fault=null;throw new IvyError('fixture_failed','Fixture write unavailable.');}
    let p;if(d.create){if(old){requireThat(hashJson(old.value)===hashJson(value),'revision_conflict','The local TaskBoard identity differs.');p=old.pin;}else p=put(key,pin(next++),value,d.create.name,d.create.parentId).pin;}
    else{requireThat(old.pin.revision===d.expectedRevision,'revision_conflict','The local workflow revision changed.');p=put(key,{...old.pin,revision:old.pin.revision+1},value,old.name,old.parent).pin;}
    if(!race&&((i.race==='begin'&&key==='task-board/native-transcript'&&d.create)||(i.race==='full'&&key==='task-board/native-full-turn-transcript'))){race=true;throw new IvyError('mutation_conflict','Fixture competing writer.');}return p;
   },
   saveLocalBlob(key,name,parent,body){trace.push(['blob-save',key,name,parent,digest(body),body.length]);let b=blobs.find(x=>x.key===key&&x.name===name&&x.parent===parent);if(b)requireThat(b.body.equals(body),'revision_conflict','The local evidence identity differs.');else{b={key,name,parent,pin:pin(900+blobs.length),body:Buffer.from(body),initial:Buffer.from(body)};blobs.push(b);}return b.pin;},
   localBlob(key,p,parent){trace.push(['blob-read',key,p,parent]);const b=blobs.find(b=>b.key===key&&b.pin.objectId===p.objectId&&b.parent===parent);requireThat(b,'task_board_evidence_mismatch','The local evidence blob is unavailable.');return b.body;},
  };
  const engine={store,settings:{principalId:'caller'},async verifyOwner(){trace.push(['owner']);if(++owners===i.failOwner)throw new IvyError('fixture_failed','owner');}};
  const turns=new Turns(store,'caller'),subject=new Collector(engine),fullCollector=new FullCollector(engine,subject.inspector);
  const action=async(method,args)=>{actions.push({method,args:structuredClone(args)});try{let value;
   if(method==='saveSnapshot')value=await turns.saveSnapshot(...args);
   else if(method==='begin')value=await subject.begin(...args);
   else if(method==='advance')value=await subject.advance(...args);
   else if(method==='directFull')value=await fullCollector.collect(args[0],document(latest('task-board/native-transcript',args[1])),args[2],args[3]);
   else if(method==='fault'){fault=args[0];value=null;}
   else if(method==='patch'){const d=latest(args[0],args[1]);for(const[k,v]of Object.entries(args[2]))d.value[k]=k==='evidence'?{...d.value[k],...v}:v;value=null;}
   else throw Error('Unknown action');results.push({ok:value});return value;
  }catch(e){if(!(e instanceof IvyError))throw new Error(row.name,{cause:e});results.push({error:{code:e.code,message:e.message,outcome:e.outcome}});return undefined;}};
  const snapshot=await action('saveSnapshot',[pin(200),observed(1,'thread/read',{threadId:'thread',includeTurns:false},meta().reply),observed(2,'thread/turns/list',turnsParams('thread',null),found().reply)]);
  const collection=await action('begin',[pin(200,2),snapshot]);
  if(collection){await action('begin',[pin(200,2),snapshot]);
   if(i.fault)await action('fault',[i.fault]);if(i.collectionPatch)await action('patch',['task-board/native-transcript',collection,i.collectionPatch]);
   if(i.directFull)await action('directFull',[pin(200,2),collection,observed(3,'thread/items/list',firstItemsParams('thread','turn'),unsupported().reply),i.budget??8]);
   else for(let pass=0;pass<(i.passes??1);pass++){await action('advance',[pin(200,2),collection.objectId,i.budget??8]);if(pass===0&&i.pagePatch){const d=latest('task-board/native-transcript',collection.objectId),patch={...i.pagePatch};if(patch.previous==='self')patch.previous=d.value.head;await action('patch',['task-board/native-result-page',d.value.head,patch]);}}
   if(i.replay)await action('advance',[pin(200,2),collection.objectId,i.budget??8]);
  }
  const documents=docs.filter((d,index)=>d.key!=='task-board/run'&&!docs.slice(index+1).some(x=>x.pin.objectId===d.pin.objectId)).map(d=>({key:d.key,pin:d.pin,value:d.value,name:d.name,parent:d.parent})).sort((a,b)=>a.pin.objectId<b.pin.objectId?-1:1);
  row.run=run;row.actions=actions;row.writeIds=writeIds;row.observations=observations;row.statusReplies=statusReplies;row.blobs=blobs.map(b=>({key:b.key,name:b.name,parent:b.parent,pin:b.pin,hex:b.initial.toString('hex')}));
  row.expectedHash=hashJson({results,trace,documents});
  if(['paged completion and idempotent replay','bounded page checkpoint resumes','exact first unsupported item falls back','full fallback resumes bounded turn search','begin competing stable identity','full competing stable identity','final turn search resumes without rereading exhausted items'].includes(row.name)){assert.ok(results.every(r=>'ok'in r),row.name+JSON.stringify(results));assert.equal(results.at(-1).ok.kind,'complete');}
 }
 groups.push({path,rows});
}
process.stdout.write(JSON.stringify(groups));
