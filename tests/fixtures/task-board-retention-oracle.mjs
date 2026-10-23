import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {DatabaseSync} from 'node:sqlite';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {canonical,hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const source=readFileSync('services/task-board/src/runtime/store.ts','utf8');
const schema=source.match(/\.exec\(`(PRAGMA journal_mode=WAL;[\s\S]*?)`\);/)[1];
const archive=readFileSync('packages/sdk/src/receipt-archive.ts','utf8');
const hasUnresolvedOutcome=new Function(stripTypeScriptTypes(archive.slice(archive.indexOf('export function hasUnresolvedOutcome'),archive.indexOf('/** Receipts share')).replace('export ',''))+'\nreturn hasUnresolvedOutcome;')();
const methods=source.slice(source.indexOf('  private async retainedLocalRoot('),source.indexOf('  async allocateTaskKey('))+
 source.slice(source.indexOf('  private localDocument<'),source.indexOf('  async read<'));
const Store=new Function('hasUnresolvedOutcome','IvyError','requireThat','hashJson','contractVersion',stripTypeScriptTypes('class Store { local; client; rootObjectId=null; historyLimits; constructor(db,client,limits){this.local=db;this.client=client;this.historyLimits=limits;}\n'+methods+'\n}')+'\nreturn Store;')(hasUnresolvedOutcome,IvyError,requireThat,hashJson,()=> '1.0.0');
const time='2026-01-02T03:04:05.006Z';
if(process.argv.includes('--configuration-control')){
 const input=JSON.parse(readFileSync(0,'utf8'));
 const configurationSource=readFileSync('services/task-board/src/runtime/configuration.ts','utf8');
 const {validateTaskBoard}=await import('../../packages/contracts/src/task-board-validation.ts');
 const helpers=configurationSource.slice(configurationSource.indexOf('export const configurationName'),configurationSource.indexOf('export function executionWithDefaults'));
 const Configuration=new Function('validateTaskBoard','requireThat',stripTypeScriptTypes(helpers.replaceAll('export ',''))+'\nreturn {initialConfiguration,checkConfiguration};')(validateTaskBoard,requireThat);
 if(input.configurationControl==='request'){
  const value=Configuration.initialConfiguration();
  value.defaults.nativeOptions={model:null,reasoningEffort:null,serviceTier:'standard'};
  const request={action:'configure',operationId:'retention-control',configuration:null,value};
  validateTaskBoard('ActionRequest',request);validateTaskBoard('DurableRequest',request);
  const engine=readFileSync('services/task-board/src/runtime/engine.ts','utf8');
  const actorSource=engine.slice(engine.indexOf('    const actor: TaskBoard.Actor ='),engine.indexOf('    if (request.action === "savePlan")'));
  const makeActor=new Function('request','context',stripTypeScriptTypes(actorSource)+'\nreturn actor;');
  const actor=makeActor.call({owner:input.owner},request,{callerPrincipalId:input.callerPrincipalId});
  const trace=[];await Configuration.checkConfiguration({client:{async request(method,args){trace.push([method,args]);return{items:[],nextCursor:null};}}},value);
  process.stdout.write(JSON.stringify({request,actor,trace}));
 }else{
  if(input.configurationControl!=='cached')throw Error('Unbound control mode');
  const engine=readFileSync('services/task-board/src/runtime/engine.ts','utf8'),start=engine.indexOf('  private async executeCached('),end=engine.indexOf('    // Complete transcripts',start);
  const prefix=engine.slice(start,end),trace=[];
  const Cached=new Function('IvyError',stripTypeScriptTypes('class Cached {coordination;constructor(coordination){this.coordination=coordination;}'+prefix+'throw Error("Only original cached terminal branch is bound");}}',{mode:'transform'})+'\nreturn Cached;')(IvyError);
  const cached=new Cached({async release(id){trace.push(id);}});
  const outcome=await cached.executeCached(input.operation);
  process.stdout.write(JSON.stringify({outcome,trace}));
 }
 process.exit(0);
}
const operation=(extra={})=>({callerPrincipalId:'caller',operationId:'request',requestHash:'sha256:original',phase:'succeeded',actor:{source:'user'},request:{},writes:[],error:null,...extra});
const document=(id='root',key='task-board/operation',value=operation(),parent=null)=>({id,key,value,parent,name:id});
const task=(state='todo',revision=2,archived=false)=>({object:{contractKey:'task-board/task',currentRevision:revision,effectivelyArchived:archived},content:{encoding:'json',value:{workflowState:state}}});
const delivery=state=>({object:{contractKey:'task-board/delivery'},content:{encoding:'json',value:{state}}});
const rows=[];
const add=(name,input={})=>rows.push({name,input:{documents:[document()],roots:['root'],limitRows:0,limitBytes:268435456,...input}});
add('within exact row limit',{limitRows:1});add('no eligible root',{roots:[]});add('closed operation');
add('graph with chunk and retained gate',{documents:[document(),document('child','task-board/native-evidence',{value:7},'root')],blobs:[['blob','task-board/native-evidence-chunk','part','child','616263']],operationGates:[['root','gate']],dependencies:[['root','missing']]});
for(const phase of ['accepted','preparing','publishing','needs_attention'])add('unfinished '+phase,{documents:[document('root','task-board/operation',operation({phase}))]});
add('original unknown outcome',{documents:[document('root','task-board/operation',operation({error:{execution:'unknown'}}))]});
add('nested uncertainty',{documents:[document(),document('child','task-board/native-evidence',{nested:[{execution:'unknown'}]},'root')]});
for(const [state,ownerPhase] of [['dispatched','succeeded'],['observed','pending'],['observed','succeeded'],['observed','failed']])add('native call '+state+'/'+ownerPhase,{documents:[document(),document('child','task-board/native-call',{state,ownerPhase},'root')]});
add('incoming nested reference',{incoming:{items:[{objectId:'root'}]}});add('explicit protected root',{protectedId:'root'});
add('external retained reference',{documents:[document(),document('child','task-board/native-evidence',{},'root'),document('outside','task-board/native-plan',{link:{objectId:'child'}})]});
add('unrelated reference permits cleanup',{documents:[document(),document('outside','task-board/native-plan',{link:'unrelated'})]});
add('pending dependent operation',{documents:[document(),document('dependent','task-board/operation',operation({phase:'accepted'}))],dependencies:[['root','dependent']]});
add('finished dependent operation',{documents:[document(),document('dependent','task-board/operation',operation({phase:'failed'}))],dependencies:[['root','dependent']]});
add('active publication gate',{gates:[['gate','root']]});
for(const [name,read] of [['todo',task()],['waiting',task('waiting')],['done',task('done')],['other revision',task('todo',3)],['archived',task('todo',2,true)],['absent',{error:'not_found'}],['unavailable',{error:'service_unavailable'}]])add('scheduler attempt '+name,{documents:[document('root','task-board/scheduler-attempt',{task:{objectId:'task',revision:2}})],remote:{task:read}});
const scheduled=operation({actor:{source:'scheduler'},request:{taskId:'task',expectedRevision:2}});
add('scheduler operation active task',{documents:[document('root','task-board/operation',scheduled)],remote:{task:task()}});
for(const [name,read] of [['confirmed',delivery('confirmed')],['pending',delivery('queued')],['wrong key',{object:{contractKey:'other'},content:{encoding:'json',value:{state:'confirmed'}}}],['wrong encoding',{object:{contractKey:'task-board/delivery'},content:{encoding:'text',value:'confirmed'}}],['absent',{error:'not_found'}],['unavailable',{error:'service_unavailable'}]])add('delivery '+name,{documents:[document('root','task-board/operation',operation({writes:[{contractKey:'task-board/delivery',outcome:{objectId:'delivery'}}]}))],remote:{delivery:read}});
add('revision changes during remote read',{documents:[document('root','task-board/operation',scheduled)],remote:{task:task('done')},race:'revision'});
add('gate acquired during remote read',{documents:[document('root','task-board/operation',scheduled)],remote:{task:task('done')},race:'gate'});
add('cleanup rollback after tombstone',{documents:[document(),document('child','task-board/native-evidence',{},'root')],fault:"CREATE TRIGGER fail_cleanup BEFORE DELETE ON revisions BEGIN SELECT RAISE(ABORT,'fixture interruption'); END;"});
add('FIFO retains newer root',{documents:[document(),document('other','task-board/operation',operation({operationId:'other'}))],roots:['root','other'],limitRows:1});
add('byte budget',{limitRows:100,limitBytes:0});add('incoming reserved row',{limitRows:1,reservedRows:1});
add('orphan cleanup marker',{documents:[]});add('generic closed graph',{documents:[document('root','task-board/native-plan',{saved:true})]});
const tables={documents:'id,key,name,parent_id,current_revision,created_at,updated_at',revisions:'id,revision,value_json,created_at',blobs:'id,key,name,parent_id,hex(bytes)',markers:'key,value',cleanup_roots:'root_id,delete_after',cleanup_dependencies:'root_id,operation_id',active_gates:'gate_id,operation_id',operation_gates:'operation_id,gate_name',workspace_reservations:'host_id,canonical_cwd,task_id,operation_id',storage_usage:'kind,rows,bytes',retired_operations:'caller_id,operation_id,request_hash,name'};
const snapshot=db=>Object.fromEntries(Object.entries(tables).map(([table,columns])=>[table,db.prepare('SELECT '+columns+' FROM '+table+' ORDER BY 1,2').all().map(row=>Object.values(row))]));
for(const row of rows){const i=row.input,db=new DatabaseSync(':memory:');db.exec(schema);
 for(const d of i.documents){db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(d.id,d.key,d.name,d.parent,1,time,time);db.prepare('INSERT INTO revisions VALUES(?,?,?,?)').run(d.id,1,canonical(d.value),time);}
 for(const [id,key,name,parent,hex]of i.blobs??[])db.prepare('INSERT INTO blobs VALUES(?,?,?,?,?)').run(id,key,name,parent,Buffer.from(hex,'hex'));
 for(const values of i.gates??[])db.prepare('INSERT INTO active_gates VALUES(?,?)').run(...values);
 for(const values of i.operationGates??[])db.prepare('INSERT INTO operation_gates VALUES(?,?)').run(...values);
 for(const root of i.roots)db.prepare('INSERT INTO cleanup_roots VALUES(?,?)').run(root,time);
 for(const values of i.dependencies??[])db.prepare('INSERT INTO cleanup_dependencies VALUES(?,?)').run(...values);
 if(i.fault)db.exec(i.fault);
 const trace=[];const client={async request(method,params){trace.push([method,params]);
  if(i.race==='revision'){const value=i.documents[0].value;db.prepare('UPDATE documents SET current_revision=2 WHERE id=?').run('root');db.prepare('INSERT INTO revisions VALUES(?,?,?,?)').run('root',2,canonical(value),time);}
  if(i.race==='gate')db.prepare('INSERT INTO active_gates VALUES(?,?)').run('gate','root');
  const reply=i.remote?.[params.objectId]??{error:'not_found'};if(reply.error)throw new IvyError(reply.error,'fixture remote');return reply;
 }};
 const store=new Store(db,client,{count:i.limitRows,bytes:i.limitBytes});let result=null;
 try{await store.collectLocal(0,i.reservedBytes??0,i.reservedRows??0,i.protectedId,i.incoming);}catch(e){result=e instanceof IvyError?{code:e.code,message:e.message}:{storageError:true};}
 row.expected={result,trace,tables:snapshot(db)};db.close();
}
if(process.argv.includes('--factory-snapshot')){
 // Replay the real factory's saved tables in an isolated database through the
 // original collector. No synthetic Operation or delivery schema is admitted.
 const input=JSON.parse(readFileSync(0,'utf8')),db=new DatabaseSync(':memory:');db.exec(schema);
 for(const table of Object.keys(tables))db.exec('DELETE FROM '+table);
 for(const [table,values]of Object.entries(input.tables)){
  if(table==='storage_usage')continue;
  for(const saved of values){const row=[...saved];if(table==='blobs')row[4]=Buffer.from(row[4],'hex');db.prepare('INSERT INTO '+table+' VALUES('+row.map(()=>'?').join(',')+')').run(...row);}
 }
 if(input.cleanupOrder){const roots=new Map(input.tables.cleanup_roots);db.exec('DELETE FROM cleanup_roots');for(const id of input.cleanupOrder)db.prepare('INSERT INTO cleanup_roots VALUES(?,?)').run(id,roots.get(id));}
 db.exec('DELETE FROM storage_usage');for(const row of input.tables.storage_usage)db.prepare('INSERT INTO storage_usage VALUES(?,?,?)').run(...row);
 const trace=[],client={async request(method,params){trace.push([method,params]);const raw=input.remote[params.objectId];if(!raw)throw new IvyError('not_found','fixture remote');if(raw.error)throw new IvyError(raw.error.code,raw.error.message,raw.error.outcome);return structuredClone(raw);}};
 const store=new Store(db,client,input.limits);let failure=null;try{await store.collectLocal(input.nowMs);}catch(e){if(!(e instanceof IvyError))throw e;failure=e;}
 const operations=readFileSync('services/task-board/src/runtime/operations.ts','utf8');
 const find=operations.slice(operations.indexOf('const operationName'),operations.indexOf('  async begin(')).replace('export class','class')+'\n}';
 const Subject=new Function('identityHash','requireThat',stripTypeScriptTypes(find,{mode:'transform'})+'\nreturn TaskBoardOperations;')((callerPrincipalId,operationId)=>hashJson({callerPrincipalId,operationId}).slice('sha256:'.length),requireThat);
 const subject=new Subject({async named(key,name){const row=db.prepare('SELECT d.id,d.current_revision,r.value_json FROM documents d JOIN revisions r ON r.id=d.id AND r.revision=d.current_revision WHERE d.key=? AND d.name=?').get(key,name);return row?{pin:{objectId:row.id,revision:row.current_revision},value:JSON.parse(row.value_json)}:null;},retiredOperation:(...args)=>store.retiredOperation(...args)});
 const lookups=[];for(const [caller,id]of input.lookups??[]){try{const found=await subject.find(caller,id);lookups.push({found:found?.pin??null});}catch(e){if(!(e instanceof IvyError))throw e;lookups.push({error:{code:e.code,message:e.message,outcome:e.outcome}});}}
 let diagnostics;if(input.diagnostic){
  const reconciler=readFileSync('services/task-board/src/runtime/reconciler.ts','utf8');
  const methods=reconciler.slice(reconciler.indexOf('  get diagnostics()'),reconciler.indexOf('  private async process('));
  class Clock extends Date{constructor(){super(input.diagnostic.now);}}
  const Diagnostic=new Function('IvyError','Date','source',stripTypeScriptTypes('class Diagnostic { failures=new Map();engine={owner:{serviceNodeId:source}};'+methods+'}')+'\nreturn Diagnostic;')(IvyError,Clock,input.diagnostic.source);
  const diagnostic=new Diagnostic();diagnostic.record('cleanup',failure??undefined);diagnostics=diagnostic.diagnostics;
 }
 process.stdout.write(JSON.stringify({trace,tables:snapshot(db),lookups,...failure?{failure:{code:failure.code,message:failure.message,outcome:failure.outcome}}:{},...diagnostics?{diagnostics}:{}}));db.close();
}else process.stdout.write(JSON.stringify({time,tables,rows}));
