import {readFileSync,existsSync} from 'node:fs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import assert from 'node:assert/strict';

registerHooks({resolve(s,c,next){
  if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){
    const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);
    if(existsSync(fileURLToPath(u)))return next(u.href,c);
  }
  return next(s,c);
}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const source=readFileSync('services/task-board/src/main.ts','utf8');
const body=source.slice(source.indexOf('export async function startTaskBoard'),source.indexOf('\nif (process.argv[1]'));
assert.ok(body.includes('Promise.all([previous.stop(), activeWork])'));
assert.ok(body.includes('.finally(() => previousEngine?.store.close())'));
const gate=()=>{let resolve;const promise=new Promise(r=>{resolve=r;});return{promise,resolve};};
const log=[],stores=[],runtimes=[],timers=new Map();let client,id=0,now=1000000;
const settings={principalId:'fixture-principal',rootObjectId:null,scheduler:{enabled:false,intervalMs:1000,pageSize:50},phoneTarget:null};
class Store {
  constructor(connection){this.generation=connection.generation;stores.push(this);log.push(['open',this.generation]);}
  close(){assert.equal(this.closed,undefined);this.closed=true;log.push(['release',this.generation]);}
}
class Engine {
  constructor(store){this.store=store;this.owner={generation:store.generation};}
  async verifyOwner(){log.push(['owner',this.owner.generation]);}
}
// Explicit finite runtime ports: the original lifecycle owns all retirement,
// callback, tick and store-release ordering. Domain publication is not mocked
// into an asserted original Operation result by this oracle.
class Runtime {
  constructor(engine){this.generation=engine.owner.generation;this.recovered=true;this.diagnostics=[];this.work=gate();this.tickWork=gate();runtimes.push(this);}
  tick(){log.push(['tick',this.generation]);return this.tickWork.promise.then(()=>{log.push(['tick.done',this.generation]);});}
  async stop(){log.push(['stop',this.generation]);await this.work.promise;log.push(['stop.done',this.generation]);}
}
class Service {
  constructor(options){this.options=options;this.ready=false;client=this;}
  start(){}
  async stop(){this.ready=false;this.options.onState({status:'stopped',generation:2});log.push(['sdk.stopped',2]);}
}
class Health {async write(){} async control(){return null;}}
const start=new Function('instanceConfig','validateTaskBoard','requireThat','HealthFile','TaskBoardStore','TaskBoardEngine','TaskBoardReconciler','ServiceClient','taskBoardTools','taskBoardRegistry','Date','setTimeout','clearTimeout','setInterval','clearInterval',
  stripTypeScriptTypes(body.replace('export async function','async function'),{mode:'transform'})+'\nreturn startTaskBoard;')(
  async()=>({componentId:'task-board',credential:'finite-token',serviceNodeId:'fixture-task-board',settings}),()=>{},requireThat,Health,Store,Engine,Runtime,Service,[{name:'workspace'}],()=>{},
  {now:()=>now},(fn,delay)=>{const token=++id;timers.set(token,{fn,delay});return token;},token=>timers.delete(token),()=>++id,()=>{});
const subject=await start('finite-instance');
const error=async work=>{try{await work();assert.fail('Late admission must fail');}catch(e){e=IvyError.from(e);return{code:e.code,message:e.message,outcome:e.outcome};}};
const context=g=>({generation:g,callerPrincipalId:'caller-\ud800',signal:{throwIfAborted(){}}});
const connection=g=>({generation:g,async request(){assert.fail('Null-root lifecycle performs no root request');}});
async function attach(g){await client.options.reconcile(connection(g));client.ready=true;client.options.onState({status:'ready',generation:g});}
const flush=async()=>{for(let i=0;i<20;i++)await Promise.resolve();};
await attach(1);assert.equal(runtimes.length,1);
client.ready=false;client.options.onState({status:'offline',generation:1});
const offline={handler:await error(()=>client.options.handlers['task-board.workspace']({},context(1))),readiness:await error(()=>client.options.readiness(connection(1)))};
let attached=false;const replacing=attach(2).then(()=>{attached=true;});await flush();
assert.equal(stores.length,1);assert.equal(stores[0].closed,undefined);assert.equal(attached,false);
runtimes[0].work.resolve();await flush();
assert.ok(log.some(v=>v[0]==='stop.done'&&v[1]===1));assert.equal(stores[0].closed,undefined);assert.equal(attached,false);
now+=300;runtimes[0].tickWork.resolve();await replacing;
assert.equal(stores[0].closed,true);assert.equal(stores.length,2);
let closed=false;const closing=subject.close().then(()=>{closed=true;});await flush();
const closingErrors={handler:await error(()=>client.options.handlers['task-board.workspace']({},context(2))),readiness:await error(()=>client.options.readiness(connection(2)))};
assert.equal(closed,false);assert.equal(stores[1].closed,undefined);
runtimes[1].tickWork.resolve();await flush();assert.equal(closed,false);assert.equal(stores[1].closed,undefined);
runtimes[1].work.resolve();await closing;assert.equal(stores[1].closed,true);
const at=(name,g)=>log.findIndex(v=>v[0]===name&&v[1]===g);
for(const g of [1,2]){assert.ok(at('stop.done',g)>=0&&at('stop.done',g)<at('release',g));assert.ok(at('tick.done',g)>=0&&at('tick.done',g)<at('release',g));}
assert.ok(at('release',1)<at('open',2));assert.deepEqual(closingErrors,offline);
// Original engine, Operations, coordination and SQLite store classify the
// accepted action after the captured connection becomes unavailable. Only
// creation policy and remote reads are finite ports; no native work is allowed.
const {TaskBoardStore:OriginalStore}=await import('../../services/task-board/src/runtime/store.ts');
const {TaskBoardEngine:OriginalEngine}=await import('../../services/task-board/src/runtime/engine.ts');
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
let unavailable=false;const rpc=[];
const originalStore=new OriginalStore({async request(method,params){
  rpc.push({method,params:structuredClone(params),unavailable});
  if(unavailable)throw new IvyError('service_unavailable','Hive service connection is unavailable.');
  if(method==='serviceNodes.get')return{serviceName:'task-board',principalId:'fixture-principal',connected:true,synced:true};
  if(method==='objects.query')return{items:[],nextCursor:null};
  throw Error('Unbound original lifecycle RPC '+method);
}},null);
const request={action:'create',operationId:'factory-create',workflowState:'review',fields:{title:'Manual factory task',description:'Original durable material',acceptanceCriteria:[],category:null,control:'user',priority:2,executionRequirement:null,workspaceRequirement:{kind:'directory_path',path:'/fixture'},dependencies:[],nextReviewAt:null,dueAt:null},initialResult:{summary:'Original final material',artifacts:[],checks:[],codeReferences:[],repositoryResult:null,limitations:[]}};
const original=new OriginalEngine(originalStore,settings,{serviceNodeId:'fixture-task-board',generation:1});
const entered=gate(),creation=gate();
original.checks.fields=async()=>{};
original.checks.initialState=async()=>{entered.resolve();await creation.promise;return{workflowState:'review',waiting:null};};
original.checks.executionOptions=async()=>{};
const invokeContext={...context(1),operationId:request.operationId};
const admitted=original.invoke(request,invokeContext).then(value=>({ok:value}),e=>({error:{code:e.code,message:e.message,outcome:e.outcome}}));
await entered.promise;
const accepted=await original.operations.find(invokeContext.callerPrincipalId,request.operationId);
assert.equal(accepted.value.phase,'accepted');assert.equal(originalStore.local.prepare('SELECT count(*) n FROM active_gates').get().n,1);
unavailable=true;creation.resolve();const refused=await admitted;
const terminal=await original.operations.find(invokeContext.callerPrincipalId,request.operationId);
assert.equal(terminal.pin.objectId,accepted.pin.objectId);assert.equal(terminal.value.phase,'failed');assert.equal(terminal.value.writes.length,0);assert.equal(terminal.value.outcome,null);
assert.deepEqual(refused.error,{code:'service_unavailable',message:'Hive service connection is unavailable.',outcome:'not_executed'});
assert.deepEqual(terminal.value.error,{code:refused.error.code,message:refused.error.message,execution:refused.error.outcome});
assert.equal(originalStore.local.prepare('SELECT count(*) n FROM active_gates').get().n,0);
for(const key of ['request','requestHash','actor','callerPrincipalId','createdAt'])assert.deepEqual(terminal.value[key],accepted.value[key]);
const revisionCount=originalStore.local.prepare('SELECT count(*) n FROM revisions').get().n;
unavailable=false;const replacement=new OriginalEngine(originalStore,settings,{serviceNodeId:'fixture-task-board',generation:2});
const replay=await replacement.invoke(request,{...invokeContext,generation:2}).then(value=>({ok:value}),e=>({error:{code:e.code,message:e.message,outcome:e.outcome}}));
assert.deepEqual(replay,refused);assert.equal(originalStore.local.prepare('SELECT count(*) n FROM revisions').get().n,revisionCount);
const domain={accepted,terminal,refused,replay,rpc,requestHash:hashJson(request),activeGates:0,unchangedReplayRevisions:true};originalStore.close();
const files=['services/task-board/src/main.ts','services/task-board/src/runtime/reconciler.ts','services/task-board/src/runtime/engine.ts','services/task-board/src/runtime/operations.ts','services/task-board/src/runtime/store.ts','services/task-board/src/runtime/coordination.ts'];
process.stdout.write(JSON.stringify({offline,closing:closingErrors,log,domain,sourcePins:Object.fromEntries(files.map(p=>[p,'sha256:'+createHash('sha256').update(readFileSync(p)).digest('hex')])),limits:'Original main lifecycle with finite config/health/SDK/engine/runtime ports, plus original engine/Operations/coordination/SQLite refusal and replay with finite creation/remote-read ports. Does not establish original public-handler drain, complete routing parity or final owner policies/contracts.'}));
