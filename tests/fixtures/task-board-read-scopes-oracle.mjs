import {readFileSync, existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {registerHooks, stripTypeScriptTypes} from 'node:module';
import {AsyncLocalStorage} from 'node:async_hooks';
import {DatabaseSync} from 'node:sqlite';
import assert from 'node:assert/strict';
registerHooks({resolve(s, c, next) {
  if (s.startsWith('.') && s.endsWith('.js') && c.parentURL) {
    const url = new URL(s.slice(0, -3) + '.ts', c.parentURL);
    if (existsSync(fileURLToPath(url))) return next(url.href, c);
  }
  return next(s, c);
}});
const {canonical, hashJson} = await import('../../packages/contracts/src/canonical.ts');
const {IvyError, requireThat} = await import('../../packages/contracts/src/errors.ts');
const source = readFileSync('services/task-board/src/runtime/store.ts', 'utf8');
const methods = source.slice(source.indexOf('  withReadCache<'), source.indexOf('  constructor(')) +
  source.slice(source.indexOf('  private localDocument<'), source.indexOf('  async writeAttachment(')) +
  source.slice(source.indexOf('  localKey('), source.indexOf('  localArtifact('));
const Store = new Function('canonical', 'hashJson', 'IvyError', 'requireThat', 'contractVersion', 'readableVersions', 'upgradeRecord', 'localContractKeys', 'taskAttachmentPins', 'pinOf', 'AsyncLocalStorage',
  stripTypeScriptTypes(`class Store { local; client; rootObjectId='root'; reads=new AsyncLocalStorage(); constructor(local,client){this.local=local;this.client=client;} ${methods} }`) + '\nreturn Store;')(
    canonical, hashJson, IvyError, requireThat, () => '1.0.0', () => ['1.0.0'], (_key, value) => value, new Set(), () => {throw Error('History has no attachment references');},
    value => ({objectId: value.object.id, revision: value.revision.revision}), AsyncLocalStorage);
const key='task-board/history', time='2026-01-02T03:04:05.000Z', value=mark=>({mark,nested:{list:[1]}}), pin=(id='remote',revision=1)=>({objectId:id,revision});
const read=(id='remote',options={})=>({method:'read',address:pin(id),...options}), scope=(...steps)=>({method:'scope',steps}), write=(id='written',revision=1)=>({method:'writePrepared',value:value('v'+revision),destination:{objectId:id,expectedRevision:revision-1}}), submit=(label,...steps)=>({method:'submit',label,steps}), drain={method:'drain'};
const cases=[], add=(name,...steps)=>cases.push({name,steps});
add('outside actions never cache',read(),read(),read());
add('nested scopes share one map',scope(read(),scope(read(),scope(read()),read()),read()),read());
add('independent actions start fresh',scope(read(),read()),scope(read(),read()),read(),scope(read()));
add('latest reads always reach Hive',scope(read(),read('remote',{address:'remote'}),read('remote',{address:'remote'}),read()));
add('cached values are cloned',scope(read('remote',{mutate:true}),scope(read('remote',{mutate:true})),read()));
add('primary and archive flags select separate keys',scope(read(),read('remote',{primary:false}),read('remote',{includeArchived:true}),read('remote',{primary:false,includeArchived:true}),read(),read('remote',{primary:false})));
add('pinned revisions select separate keys',scope(read(),read('remote',{address:pin('remote',2)}),read(),read('remote',{address:pin('remote',2)})));
add('archive errors do not enter cache',scope(read('archived'),read('archived'),read('archived',{includeArchived:true}),read('archived',{includeArchived:true}),read('archived')));
add('scope errors do not enter cache',scope(read('other-root'),read('other-root',{primary:false}),read('other-root'),read('other-root',{primary:false})));
add('pin errors do not enter cache',scope(read('wrong-pin'),read('wrong-pin'),read()));
add('missing records do not enter cache',scope(read('missing'),read('missing'),scope(read(),read())));
for(const size of [65536,65537])add('read byte bound '+size,scope(read('size-'+size),read('size-'+size),scope(read('size-'+size))));
for(const count of [255,256,257])add('read count bound '+count,scope(...Array.from({length:count},(_,i)=>read('n'+i)),read('n0'),read('n'+(count-1)),read('extra'),read('extra')));
add('write invalidates only the action map',scope(read(),write('remote',2),read(),read('remote',{address:pin('remote',2)}),read('remote',{address:pin('remote',2)}),read('remote',{address:'remote'})),scope(read('remote',{address:pin('remote',2)})));
add('write seed is not a pinned read key',scope(write(),read('written'),read('written')));
for(const count of [255,256,257])add('write seed occupies bounded slots '+count,scope(...Array.from({length:count},(_,i)=>write('written-'+i)),read(),read(),read('extra'),read('extra')));
add('rewrite invalidates both write and read keys',scope(write('remote',1),read(),read(),write('remote',2),read(),read('remote',{address:pin('remote',2)}),read('remote',{address:pin('remote',2)})));
add('failed write leaves pinned entries',scope(read(),write('write-error'),read(),read('write-error'),write('write-unknown'),read()));
add('local records bypass action cache',scope(read('local'),read('local'),scope(read('local'))),read('local'));
add('overlapping root actions are isolated',{method:'parallel',steps:[scope(read(),read()),scope(read(),scope(read(),read()))]},read());
add('parallel nested work shares completed pins',scope(read(),{method:'parallel',steps:[scope(read(),read()),scope(read(),read())]},read()));
add('pending duplicate reads are not coalesced',scope({method:'parallel',steps:[scope(read(),read()),scope(read(),read())]},read()));
add('independent write does not invalidate peer cache',{method:'parallel',steps:[scope(read(),read(),read()),scope(write('remote',2),read('remote',{address:pin('remote',2)}),read('remote',{address:pin('remote',2)}))]});
add('submitted work retains ended caller map',scope(read(),submit('inside',read(),read())),read(),drain,read(),scope(read(),read()));
add('submitted work outside action stays uncached',submit('outside',read(),read()),scope(read(),drain,read()),read());
add('two ended callers retain distinct maps',scope(read(),submit('first',read(),read())),scope(read(),submit('second',read(),read())),drain,read());
add('uncached submitted work creates its own nested action',submit('outside',scope(read(),read())),scope(read(),drain,read()));
const results=[];
for(const spec of cases){
  const db=new DatabaseSync(':memory:');
  db.exec(source.match(/\.exec\(`(PRAGMA journal_mode=WAL;[\s\S]*?)`\);/)[1]);
  db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run('local',key,'Local','root',1,time,time);
  db.prepare('INSERT INTO revisions VALUES(?,?,?,?)').run('local',1,canonical(value('v1')),time);
  const boundaries=[], heads=new Map(), records=new Map(), queued=[], completed=[];
  const error=e=>{const v=IvyError.from(e);return{code:v.code,message:v.message,outcome:v.outcome,details:v.details??null};};
  const response=(id,revision,v,byteLength=Buffer.byteLength(canonical(v)))=>({object:{id,parentId:id==='other-root'?'other':'root',ownerObjectId:null,name:id,path:'',position:0,icon:null,contractKey:key,currentRevision:heads.get(id)??revision,contractVersion:'1.0.0',archivedAt:null,effectivelyArchived:id==='archived',createdAt:time,updatedAt:time},revision:{objectId:id,revision,contractVersion:'1.0.0',mediaType:'application/json',byteLength,contentHash:hashJson(v),createdAt:time,references:{}},content:{encoding:'json',value:v}});
  const client={async request(method,args){const frame={method,args:structuredClone(args)};boundaries.push(frame);await Promise.resolve();try{
    let r;if(method==='objects.read'){
      const id=args.objectId;if(id==='missing')throw new IvyError('not_found','Fixture record is missing.');
      const revision=id==='wrong-pin'?2:args.revision??heads.get(id)??1,v=records.get(id+':'+revision)??value('v'+revision);
      r=response(id,revision,v,id.startsWith('size-')?Number(id.slice(5)):undefined);
    }else if(method==='objects.write'){
      if(args.objectId.startsWith('write-'))throw new IvyError('service_unavailable','Fixture write failed.',args.objectId==='write-unknown'?'unknown':'not_executed');
      const revision=args.expectedRevision+1;heads.set(args.objectId,revision);records.set(args.objectId+':'+revision,structuredClone(args.content.value));r=response(args.objectId,revision,args.content.value);delete r.content;
    }else throw Error('Unexpected RPC '+method);
    frame.value=structuredClone(r);return r;
  }catch(e){frame.error=error(e);throw e;}}};
  const store=new Store(db,client), project=d=>({pin:d.pin,value:d.value,parentId:d.metadata.parentId,contentHash:d.revision.contentHash});
  const run=async steps=>{const outcomes=[];for(const step of steps){
    if(step.method==='scope'){outcomes.push(await store.withReadCache(()=>run(step.steps)));continue;}
    if(step.method==='parallel'){outcomes.push(await Promise.all(step.steps.map(step=>run([step]))));continue;}
    if(step.method==='submit'){const gate=Promise.withResolvers(),job=gate.promise.then(async()=>{completed.push({label:step.label,outcomes:await run(step.steps)});});queued.push({gate,job});outcomes.push(null);continue;}
    if(step.method==='drain'){for(const {gate,job}of queued.splice(0)){gate.resolve();await job;}outcomes.push(structuredClone(completed));continue;}
    try{const selected=step.method==='read'?await store.read(key,step.address,step.primary??true,step.includeArchived??false):await store.writePrepared(key,step.value,'retained-cache-fixture',step.destination);outcomes.push({ok:structuredClone(step.method==='read'?project(selected):selected)});if(step.mutate){selected.value.mark='caller mutation';selected.value.nested.list.push(2);}}
    catch(e){outcomes.push({error:error(e)});}
  }return outcomes;};
  try{const expected={outcomes:await run(spec.steps)};assert.equal(queued.length,0,'Every submitted job must be drained');const allowed=[value('v1'),value('v2')].map(hashJson);results.push({...spec,allowed,boundaries,expected,expectedHash:hashJson(expected)});}finally{db.close();}
}
const row=name=>results.find(r=>r.name===name),calls=name=>row(name).boundaries.filter(b=>b.method==='objects.read').length;
assert.equal(calls('nested scopes share one map'),2);
assert.equal(calls('outside actions never cache'),3);
assert.equal(calls('overlapping root actions are isolated'),3);
assert.equal(calls('parallel nested work shares completed pins'),1);
assert.equal(calls('pending duplicate reads are not coalesced'),2);
assert.equal(calls('submitted work retains ended caller map'),4);
assert.equal(calls('submitted work outside action stays uncached'),4);
assert.equal(calls('write seed is not a pinned read key'),1);
assert.equal(calls('write seed occupies bounded slots 255'),3);
assert.equal(calls('write seed occupies bounded slots 256'),4);
process.stdout.write(JSON.stringify(results));
