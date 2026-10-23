import{readFileSync,existsSync}from'node:fs';import{registerHooks,stripTypeScriptTypes}from'node:module';import{fileURLToPath}from'node:url';import assert from'node:assert/strict';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const{IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');const{hashJson,canonical}=await import('../../packages/contracts/src/canonical.ts');
const source=readFileSync('services/task-board/src/runtime/reconciler.ts','utf8');
// Retain the legacy fairness/error comparison with the current admitted state
// selection. Direct-turn discovery is exercised against native Rust separately.
const code=stripTypeScriptTypes(source.slice(source.indexOf('type Lane')).replace('export class','class').replace('{ op: "eq", field: "data:/fields/control", value: "agent" },','').replace('["todo", "waiting", "review"]','["backlog", "todo", "in_progress", "waiting", "review", "done", "cancelled"]'),{mode:'transform'});
const id=n=>'00000000-0000-4000-8000-'+String(n).padStart(12,'0'),types=['operations','runs','tasks','deliveries'];
const cycles=n=>Array.from({length:n},()=>[['tick'],['flush']]).flat(),rows=[],add=(name,input={},actions=cycles(14))=>rows.push({name,input,actions});
add('startup precedes fair round robin',{counts:[2,2,2,2]});add('empty startup',{counts:[0,0,0,0]});
add('priority query and bounded cursors',{counts:[3,3,5,3],pageSize:1},cycles(30));
add('unrelated tasks use free slots',{counts:[0,0,2,0]},[...cycles(4),['tick'],['tick'],['tick'],['release',id(100)],...cycles(8)]);
add('ten task ceiling resumes pending page',{counts:[0,0,12,0]},[...cycles(4),['tick'],['tick'],['tick'],['release',id(100)],['tick'],['tick'],['tick'],['tick'],['beginStop'],['tick'],['flush'],['finishStop']]);
add('same task is serialized across startup rows',{counts:[3,0,0,0],sameTask:true},[['tick'],['tick'],['tick'],['flush'],...cycles(8)]);
add('busy task is revisited during startup',{counts:[1,0,0,0],gates:1},[['tick'],['tick'],['flush'],...cycles(6)]);
add('gates wrap after final retained cursor',{counts:[0,0,0,0],gates:3},cycles(10));
add('gates obey same task ceiling',{counts:[0,0,0,0],gates:12},[['tick'],['tick'],['release',id(100)],['tick'],['beginStop'],['flush'],['finishStop']]);
add('unscoped operations share a lane key',{counts:[3,0,0,0],unscoped:true},cycles(10));
add('unknown input does not prevent native drain',{counts:[0,1,0,0],fault:{where:'sync',code:'service_unavailable',always:true}});
add('native backoff retains prior diagnostic',{counts:[0,1,0,0],fault:{where:'drain',code:'fixture_failed'},backoffAfterFault:true});
add('signals failures do not hide native progress',{counts:[0,1,0,0],fault:{where:'signals',code:'service_unavailable',always:true}});
add('signals error clears on later success',{counts:[0,1,0,0],fault:{where:'signals',code:'service_unavailable'}});
add('retained backoff skips signals',{counts:[0,1,0,0],backoff:true});
for(const code of ['revision_conflict','task_board_publication_busy','service_unavailable']) {
 add('operation error '+code,{counts:[1,0,0,0],fault:{where:'recover',code,always:true}});
 add('gate error '+code,{counts:[0,0,0,0],gates:1,fault:{where:'recover',code,execution:'unknown',always:true}});
}
add('matching terminal refusal clears recovery fault',{counts:[1,0,0,0],terminal:true,fault:{where:'recover',code:'not_found',always:true}});
add('different terminal refusal stays visible',{counts:[1,0,0,0],terminal:true,terminalCode:'different',fault:{where:'recover',code:'not_found',always:true}});
add('terminal unknown remains unresolved',{counts:[1,0,0,0],terminal:true,fault:{where:'recover',code:'not_found',execution:'unknown',always:true}});
add('gate task read failure keeps publication detail',{counts:[0,0,0,0],gates:1,fault:{where:'read',code:'service_unavailable',always:true}});
add('failed scan key falls back to row identity',{counts:[0,1,0,0],fault:{where:'read',code:'not_found',always:true}});
for(const where of ['owner','collect','free','page:operations','gates','schedule','delivery'])add('boundary failure '+where,{counts:[1,1,1,1],fault:{where,code:'service_unavailable'}});
add('low storage warns without inventing a scheduler blocker',{counts:[0,0,1,0],freeBytes:1073741823});
add('one GiB is inclusive',{counts:[0,0,0,0],freeBytes:1073741824});add('unknown storage has no warning',{counts:[0,0,0,0],freeBytes:null});
add('cleanup cadence and diagnostic time',{counts:[0,0,0,0],freeBytes:1},[['tick'],['advance',59999],['tick'],['advance',1],['tick'],['advance',60000],['tick']]);
add('cursor restart is bounded',{counts:[3,0,0,0],pageSize:1,invalidPage:2},cycles(14));
add('cursor regression is refused',{counts:[3,0,0,0],pageSize:1,regressPage:2},cycles(14));
add('diagnostics bounded and UTF16 clipped',{counts:[40,0,0,0],pageSize:50,fault:{where:'recover',code:'fixture_failed',message:'a'.repeat(2047)+'😄tail',always:true}},cycles(12));
add('task change invalidates only task scan',{counts:[0,0,3,0],pageSize:1},[...cycles(5),['taskChanged'],...cycles(12)]);
for(const mode of ['change','stop','busy'])add('inflight page '+mode,{counts:[0,0,2,0],pausePage:true,pageSize:1},[
 ...cycles(4),['beginTick'],...(mode==='change'?[['taskChanged']]:mode==='stop'?[['beginStop']]:[['tick']]),
 ['releasePage'],['finishTick'],['flush'],...(mode==='stop'?[['finishStop']]:cycles(8))]);
add('stop rejects later tick admissions',{counts:[0,0,2,0]},[...cycles(4),['tick'],['beginStop'],['tick'],['flush'],['finishStop'],['tick']]);

for(const row of rows){
 const i=row.input,counts=i.counts??[0,0,0,0],trace=[],holds=[],used=new Map();let elapsed=0,faultUsed=false,backoff=false,pagePaused=false,pageRelease=null;
 let pendingTick=null,pendingStop=null,stopDone=false;
 class Clock extends Date{constructor(...args){super(...(args.length?args:[Date.parse('2026-01-02T00:00:00.000Z')+elapsed]));}static now(){return Date.parse('2026-01-02T00:00:00.000Z')+elapsed;}}
 const fail=where=>{if(i.fault?.where===where&&(!faultUsed||i.fault.always)){faultUsed=true;if(where==='drain')backoff=true;throw new IvyError(i.fault.code,i.fault.message??'Fixture failure.',i.fault.execution??'not_executed');}};
 const taskId=n=>id(100+(i.sameTask?0:n));const ids=types.map((_,l)=>Array.from({length:counts[l]},(_,n)=>id((l===0?200:l===1?300:l===2?100:400)+n)));
 const records=new Map();
 for(let l=0;l<4;l++)for(let n=0;n<ids[l].length;n++)records.set(ids[l][n],l===0?{request:i.unscoped?{}:{taskId:taskId(n)},phase:i.terminal?'failed':'accepted',error:i.terminal?{execution:'not_executed',code:i.terminalCode??i.fault?.code,message:i.fault?.message??'Fixture failure.'}:null}:{taskId:taskId(n)});
 const gates=Array.from({length:i.gates??0},(_,n)=>({gateId:id(500+n),operationId:id(200+n)}));for(let n=0;n<gates.length;n++)if(!records.has(id(200+n)))records.set(id(200+n),{request:{taskId:taskId(n)},phase:'accepted',error:null});
 const work=async(kind,key)=>{trace.push([kind,key]);await new Promise(resolve=>holds.push({task:records.get(key)?.taskId??records.get(key)?.request?.taskId??key,resolve}));fail(kind);if(kind==='drain')return !(i.backoff||(i.backoffAfterFault&&backoff));};
 const engine={owner:{serviceNodeId:'task-board-node'},settings:{scheduler:{pageSize:i.pageSize??50}},
  async verifyOwner(){trace.push(['owner']);fail('owner');},async recoverOperation(key){await work('recover',key);},
  store:{async read(key,address){trace.push(['read',key,address]);fail('read');assert.ok(records.has(address));return{pin:{objectId:address,revision:1},value:structuredClone(records.get(address)),metadata:{parentId:'root'}};},
   async page(key,options){trace.push(['page',key,structuredClone(options)]);const l=types.indexOf(key.slice('task-board/'.length).replace('operation','operations').replace('run','runs').replace('task','tasks').replace('delivery','deliveries'));
    fail('page:'+types[l]);const count=(used.get('page:'+l)??0)+1;used.set('page:'+l,count);
    if(l===2&&i.pausePage&&!pagePaused){pagePaused=true;await new Promise(resolve=>{pageRelease=resolve;});}
    if(l===0&&count===i.invalidPage)throw new IvyError('invalid_cursor','Fixture cursor expired.');
    const start=Number(options.cursor??0),limit=options.limit;return{items:ids[l].slice(start,start+limit).map(objectId=>({objectId,revision:1})),nextCursor:l===0&&count===i.regressPage?options.cursor:(start+limit<ids[l].length?String(start+limit):null)};
   },async collectLocal(){trace.push(['collect']);fail('collect');},localUsage(){trace.push(['free']);fail('free');return{freeBytes:'freeBytes'in i?i.freeBytes:1073741824};},
   pendingCoordinationPage(after,limit){trace.push(['gates',after,limit]);fail('gates');return gates.filter(g=>!after||g.gateId>after).slice(0,limit);}},
  native:{drain:key=>work('drain',key)},signals:{step:key=>work('signals',key)},scheduler:{task:key=>work('schedule',key)}};
 class Native{constructor(){return engine.native;}}class Signals{constructor(){return engine.signals;}}class Scheduler{constructor(){return engine.scheduler;}}class Deliveries{async step(key){await work('delivery',key);}}
 const Reconciler=new Function('IvyError','requireThat','TaskBoardNativeOrchestrator','TaskBoardScheduler','TaskBoardNativeSignals','TaskBoardDeliveries','syncNativeComments','Date',code+'\nreturn TaskBoardReconciler;')(IvyError,requireThat,Native,Scheduler,Signals,Deliveries,(_e,key)=>work('sync',key),Clock);
 const subject=new Reconciler(engine),observations=[];const pump=()=>new Promise(resolve=>setImmediate(resolve));
 for(const action of row.actions){
  if(action[0]==='tick')await subject.tick();
  else if(action[0]==='beginTick'){assert.equal(pendingTick,null);pendingTick=subject.tick();}
  else if(action[0]==='finishTick'){await pendingTick;pendingTick=null;}
  else if(action[0]==='beginStop')pendingStop=subject.stop().then(()=>{stopDone=true;});
  else if(action[0]==='finishStop'){await pendingStop;pendingStop=null;}
  else if(action[0]==='taskChanged')subject.taskChanged();
  else if(action[0]==='advance')elapsed+=action[1];
  else if(action[0]==='releasePage'){assert.ok(pageRelease);pageRelease();pageRelease=null;}
  else if(action[0]==='release'){for(const h of holds.filter(h=>h.task===action[1]))h.resolve();holds.splice(0,holds.length,...holds.filter(h=>h.task!==action[1]));}
  else if(action[0]==='flush'){for(let n=0;n<20;n++){for(const h of holds.splice(0))h.resolve();await pump();if(!holds.length)break;}await subject.settled();}
  await pump();
  // Independent workers may interleave; compare every call and its arguments
  // as a multiset per command, plus each row's strictly ordered work events.
  const batch=trace.splice(0),workEvents=batch.filter(e=>['sync','drain','signals','recover','schedule','delivery'].includes(e[0]));
  const workOrder=[...new Set(workEvents.map(e=>e[1]))].sort().map(key=>[key,workEvents.filter(e=>e[1]===key).map(e=>e[0])]);
  const calls=batch.filter(e=>!workEvents.includes(e)).sort((a,b)=>hashJson(a)<hashJson(b)?-1:hashJson(a)>hashJson(b)?1:0);
  observations.push({recovered:subject.recovered,diagnostics:subject.diagnostics,holds:holds.map(h=>h.task).sort(),stopDone,calls,workOrder,millis:Clock.now(),at:new Clock().toISOString()});
 }
 for(const h of holds.splice(0))h.resolve();await pump();assert.equal(holds.length,0,row.name+' left a blocked worker');await subject.stop();
 row.observations=observations;row.expectedHash=hashJson(observations);
}
assert.ok(rows.find(r=>r.name==='ten task ceiling resumes pending page').observations.some(o=>o.holds.length===10));
assert.ok(rows.find(r=>r.name==='startup precedes fair round robin').observations.at(-1).recovered);
process.stdout.write(JSON.stringify(rows));
