import {readFileSync} from 'node:fs';
import {stripTypeScriptTypes} from 'node:module';
import {DatabaseSync} from 'node:sqlite';
import {win32} from 'node:path';
const source=readFileSync('services/task-board/src/runtime/store.ts','utf8');
const schema=source.match(/\.exec\(`(PRAGMA journal_mode=WAL;[\s\S]*?)`\);/)[1];
const keySource=source.slice(source.indexOf('const workspaceKey ='),source.indexOf('/** Generic persistence'));
const originalKey=new Function('win32',stripTypeScriptTypes(keySource)+'\nreturn workspaceKey;')(win32);
const methods=source.slice(source.indexOf('  private workspaceReservation('),source.indexOf('  private localDocument<'));
class IvyError extends Error{constructor(code,message){super(message);this.code=code;}}
const Store=new Function('workspaceKey','IvyError',stripTypeScriptTypes('class Store{local;client;read;constructor(db,read,client){this.local=db;this.read=read;this.client=client;}\n'+methods+'\n}')+'\nreturn Store;');
const task=(state='done',extra={})=>({pin:{objectId:'owner',revision:3},metadata:{effectivelyArchived:false},value:{workflowState:state,claim:null,publication:null,lastRun:null,...extra}});
const base=['host','C:\\Workspace','owner','original'];
const rows=[];const add=(name,input={})=>rows.push({name,input:{action:'owner',host:'host',cwd:'c:/workspace',task:'candidate',operation:'new',reservations:[base],reads:{owner:task()},stat:{currentRevision:3},...input}});
add('new reservation',{action:'reserve',reservations:[]});add('reservation conflict',{action:'reserve'});
add('same task retains operation',{action:'reserve',task:'owner'});
add('move own reservation',{action:'reserve',task:'owner',cwd:'C:\\Elsewhere'});
add('move own host',{action:'reserve',task:'owner',host:'other'});
add('independent host',{action:'reserve',host:'other'});
add('insert rollback after move',{action:'reserve',task:'owner',cwd:'C:\\Elsewhere',fault:"CREATE TRIGGER fail_reservation BEFORE INSERT ON workspace_reservations BEGIN SELECT RAISE(ABORT,'fixture interruption'); END;"});
add('own reservation does not read',{task:'owner'});add('no reservation does not read',{reservations:[]});
for(const state of ['done','cancelled','backlog','todo','waiting','review','in_progress'])add('task state '+state,{reads:{owner:task(state)}});
add('active claim',{reads:{owner:task('done',{claim:{run:'active'}})}});add('publication marker',{reads:{owner:task('done',{publication:{}})}});
add('archived owner',{reads:{owner:{...task(),metadata:{effectivelyArchived:true}}}});
add('missing owner',{reads:{owner:{error:'not_found'}}});add('owner read unavailable',{reads:{owner:{error:'service_unavailable'}}});
for(const phase of ['running','completed','failed','cancelled'])add('last run '+phase,{reads:{owner:task('done',{lastRun:{objectId:'run',revision:1}}),run:{pin:{objectId:'run',revision:1},metadata:{effectivelyArchived:false},value:{phase}}}});
add('run read unavailable',{reads:{owner:task('done',{lastRun:{objectId:'run',revision:1}}),run:{error:'not_found'}}});
add('revision changes',{stat:{currentRevision:4}});add('stat unavailable',{stat:{error:'service_unavailable'}});
add('publication gate',{gate:true});add('operation changes during stat',{race:'operation'});add('owner changes during stat',{race:'owner'});
add('available inactive owner',{action:'available'});add('available active owner',{action:'available',reads:{owner:task('waiting')}});
add('release task',{action:'release',task:'owner'});add('release other task',{action:'release'});
add('release original operation',{action:'releaseOperation',operation:'original'});add('release other operation',{action:'releaseOperation'});
for(const key of ['project:host:project','directory:host:project'])add('shared '+key,{cwd:key,reservations:[['host',key,'owner','original']],reads:{owner:task('todo')}});
for(const cwd of ['C:\\Work\\..\\Workspace','C:\\Workspace\\','\\\\server\\share\\folder','\\\\?\\C:\\Workspace','/Case/Sensitive','c:relative','Directory:host:value'])add('key '+cwd,{action:'reserve',cwd,reservations:[]});
for(const row of rows){const i=row.input,db=new DatabaseSync(':memory:');db.exec(schema);
 for(const values of i.reservations)db.prepare('INSERT INTO workspace_reservations VALUES(?,?,?,?)').run(...values);
 if(i.gate){db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run('gate','task-board/coordination','TaskBoard publication gate owner',null,1,'time','time');db.prepare('INSERT INTO active_gates VALUES(?,?)').run('gate','operation');}
 if(i.fault)db.exec(i.fault);
 const keys=new Map(),key=v=>{const normalized=originalKey(v);keys.set(v,normalized);return normalized;};
 const trace=[];const read=async(key,address,primary=true,includeArchived=false)=>{trace.push(['read',key,address,primary,includeArchived]);const id=typeof address==='string'?address:address.objectId;const r=i.reads[id]??{error:'not_found'};if(r.error)throw new IvyError(r.error,'fixture read');return r;};
 const client={async request(method,args){trace.push([method,args]);if(i.race==='operation')db.prepare('UPDATE workspace_reservations SET operation_id=?').run('replacement');if(i.race==='owner')db.prepare('UPDATE workspace_reservations SET task_id=?').run('replacement');if(i.stat.error)throw new IvyError(i.stat.error,'fixture stat');return i.stat;}};
 const Subject=Store(key,IvyError),subject=new Subject(db,read,client);let result=null;
 try{if(i.action==='reserve')result=subject.reserveWorkspace(i.host,i.cwd,i.task,i.operation);else if(i.action==='owner')result=await subject.workspaceOwner(i.host,i.cwd,i.task);else if(i.action==='available')result=await subject.reserveAvailableWorkspace(i.host,i.cwd,i.task,i.operation);else if(i.action==='release')subject.releaseWorkspace(i.task);else subject.releaseWorkspaceOperation(i.operation);}catch(e){result=e instanceof IvyError?{code:e.code,message:e.message}:{storageError:true};}
 row.keys=[...keys];row.expected={result,trace,rows:db.prepare('SELECT * FROM workspace_reservations ORDER BY host_id,canonical_cwd').all().map(Object.values)};db.close();
}
process.stdout.write(JSON.stringify(rows));
