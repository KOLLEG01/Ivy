import assert from 'node:assert/strict';
import {readFileSync,existsSync} from 'node:fs';
import {mkdir,writeFile,readFile,readdir,utimes,rm,symlink} from 'node:fs/promises';
import {registerHooks,createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
import {tmpdir} from 'node:os';
import {createHash} from 'node:crypto';
const input=JSON.parse(readFileSync(0,'utf8'));
const boundary=path.resolve(input.fixtureRoot),root=path.resolve(input.root);
assert(path.basename(boundary).startsWith('ivy-config-'));
assert(path.dirname(root)===boundary);
const RealDate=Date;
globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[input.clock]));}static now(){return input.clock;}};
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')){const ts=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(ts)))return next(ts.href,context);}return next(specifier,context);}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {HostJournal,ExecutorLock}=await import('../../packages/host-runtime/src/journal.ts');
const {atomicJson}=await import('../../packages/host-runtime/src/config.ts');
const {configurationBootstrapPlan,retainBootstrapPlan}=await import('../../packages/host-runtime/src/bootstrap.ts');
const {savePreparation,preparationIdForBuild}=await import('../../packages/host-runtime/src/preparations.ts');
const {collectHostStorage}=await import('../../packages/host-runtime/src/storage-retention.ts');
const {hostStorageSpace}=await import('../../packages/host-runtime/src/storage-space.ts');
const wrap=async fn=>{try{return{ok:true,value:(await fn())??null};}catch(e){return{ok:false,error:IvyError.from(e).toWire()};}};
const names=async p=>(await readdir(p).catch(e=>{if(e.code==='ENOENT')return[];throw e;})).sort();
const contentsHash=p=>'sha256:'+createHash('sha256').update(readFileSync(p)).digest('hex');
const setupPath=path.join(root,'test-setup.json');
if(input.kind==='setup'){
  // Reuse the actual pre-existing source fixtures, not a collector rewrite.
  const testSource=readFileSync(new URL('../storage-retention.test.ts',import.meta.url),'utf8');
  const ts=createRequire(import.meta.url)(process.env.IVY_TEST_TYPESCRIPT_MODULE??'typescript');
  const ast=ts.createSourceFile('storage-retention.test.ts',testSource,ts.ScriptTarget.ESNext,true);
  const source=ast.statements.filter(n=>ts.isFunctionDeclaration(n)&&['fixture','bootstrapFixture'].includes(n.name?.text)).map(n=>n.getText(ast)).join('\n');
  assert(source.includes('async function fixture(')&&source.includes('async function bootstrapFixture('));
  const js=ts.transpileModule(source+'\nreturn {fixture,bootstrapFixture};',{compilerOptions:{target:ts.ScriptTarget.ESNext}}).outputText;
  const factory=new Function('mkdtemp','join','tmpdir','HostJournal','rm','mkdir','writeFile','utimes','configurationBootstrapPlan','retainBootstrapPlan','atomicJson','hashJson',js)(async()=>{await mkdir(root,{recursive:true});return root;},path.join,tmpdir,HostJournal,rm,mkdir,writeFile,utimes,configurationBootstrapPlan,retainBootstrapPlan,atomicJson,hashJson);
  const f=await (input.case.startsWith('bootstrap')||input.case==='maintenance-ring'?factory.bootstrapFixture:factory.fixture)({after(){}});
  const {config,journal,candidates,old}=f;let held=null;
  const snapshotId='00000000-0000-5000-8000-000000000002';
  const snapshot={snapshotId,sourceRoot:path.join(config.stagingRoot,'sources',snapshotId,'source'),originalRoot:path.join(root,'checkout'),manifestPath:path.join(config.stagingRoot,'sources',snapshotId,'snapshot.json'),capturedAt:old.toISOString()};
  const makeSnapshot=async()=>{await mkdir(snapshot.sourceRoot,{recursive:true});await atomicJson(snapshot.manifestPath,{schemaVersion:1,snapshot});await utimes(path.dirname(snapshot.sourceRoot),old,old);};
  const makeRequest=(id,next=f.plans[3])=>({schemaVersion:1,operationId:id,next,previous:{schemaVersion:1,hostId:config.hostId,installationId:next.installationId,configPath:next.configPath,configurationHash:next.configHash,observedAt:new Date().toISOString(),owners:f.plans[0].processes.map(p=>({instanceId:p.instanceId,componentId:p.componentId,name:p.name,candidateId:p.candidateId??f.plans[0].candidateId,definitionHash:hashJson(p),enabled:true,running:false}))}});
  if(input.case==='basic'){
    journal.db.prepare('INSERT INTO installed VALUES (?,?)').run('installed-fixture',JSON.stringify({candidateId:candidates[0].candidateId}));
    journal.db.prepare('INSERT INTO runtime_targets VALUES (?,?)').run('target-fixture',JSON.stringify({candidateId:candidates[2].candidateId}));
    await makeSnapshot();const id=snapshotId;await mkdir(path.join(config.stagingRoot,'build-'+id),{recursive:true});
    await savePreparation(config,{schemaVersion:1,preparationId:id,componentId:'fixture',buildId:candidates[1].buildId,snapshot,phase:'compacted',candidate:candidates[1],startedAt:old.toISOString(),updatedAt:old.toISOString(),errorCode:null});
  }else if(input.case==='unknown-preparation'){
    await makeSnapshot();const dir=path.join(config.stagingRoot,'build-'+preparationIdForBuild(candidates[1].buildId));await mkdir(dir,{recursive:true});await writeFile(path.join(dir,'preparation.json'),'{bad');
  }else if(input.case==='retired-staging'){
    await mkdir(path.join(config.stagingRoot,'.retiring-build-'+snapshotId),{recursive:true});await mkdir(path.join(config.stagingRoot,'sources','.retiring-'+snapshotId),{recursive:true});
    await writeFile(path.join(config.stagingRoot,'.retiring-build-'+snapshotId,'leftover'),'partial');
    await writeFile(path.join(config.stagingRoot,'sources','.retiring-'+snapshotId,'leftover'),'partial');
  }else if(input.case==='retired-link'){
    const kept=path.join(root,'kept');await mkdir(kept,{recursive:true});await writeFile(path.join(kept,'sentinel'),'must remain');
    await mkdir(path.join(config.stagingRoot,'sources'),{recursive:true});
    await symlink(kept,path.join(config.stagingRoot,'.retiring-build-'+snapshotId),process.platform==='win32'?'junction':'dir');
    await symlink(kept,path.join(config.stagingRoot,'sources','.retiring-'+snapshotId),process.platform==='win32'?'junction':'dir');
  }else if(input.case==='pending-retirement'||input.case==='pending-unowned'){
    assert(journal.retireCandidate(candidates[1].candidateId));
    if(input.case==='pending-unowned'){const bad={...candidates[1],artifactRoot:path.join(root,'unowned','artifact')};journal.db.prepare('UPDATE candidate_retirements SET candidate_json=? WHERE candidate_id=?').run(JSON.stringify(bad),bad.candidateId);}
  }else if(input.case==='busy-candidate'){held=path.join(config.stagingRoot,'build-locks',candidates[3].candidateId.slice(7));
  }else if(input.case==='busy-bootstrap'){held=path.join(config.runtimeRoot,'bootstrap-maintenance-lock');
  }else if(input.case==='unknown-version'){
    const manifest=journal.manifest(candidates[1].candidateId);manifest.version='future-version';journal.db.prepare('UPDATE candidates SET manifest_json=? WHERE candidate_id=?').run(JSON.stringify(manifest),candidates[1].candidateId);
  }else if(['completed-unknown','unresolved-unknown'].includes(input.case)){
    await atomicJson(path.join(config.runtimeRoot,'bootstrap-maintenance',hashJson('unknown-history').slice(7),'record.json'),{phase:input.case==='completed-unknown'?'succeeded':'applying'});
  }else if(input.case==='reason-cap'){
    for(let n=1;n<=80;n++){const buildId='sha256:'+n.toString(16).padStart(2,'0').repeat(32),id=preparationIdForBuild(buildId);await mkdir(path.join(config.stagingRoot,'build-'+id),{recursive:true});await savePreparation(config,{schemaVersion:1,preparationId:id,componentId:'fixture',buildId,snapshot,phase:'failed',candidate:null,startedAt:old.toISOString(),updatedAt:old.toISOString(),errorCode:'fixture-error-'+n});}
  }else if(input.case==='bootstrap-order'){
    // A current Windows two-owner plan and older shared plans expose the
    // insertion-order dependency: x then y can retain one covering OS plan.
    assert.equal(process.platform,'win32');
    const id='sha256:'+'0'.repeat(64),dir=path.join(config.artifactRoot,'candidates',id.slice(7));
    const candidate={...candidates[0],candidateId:id,buildId:id,componentId:'service-manager',artifactRoot:path.join(dir,'artifact'),manifestPath:path.join(dir,'component.json')};
    const manifest={...journal.manifest(candidates[0].candidateId),componentId:'service-manager',buildId:id};
    await mkdir(candidate.artifactRoot,{recursive:true});await writeFile(candidate.manifestPath,JSON.stringify(manifest));await utimes(dir,old,old);journal.saveCandidate(candidate,manifest);
    config.instances.push({instanceId:'service-manager',componentId:'service-manager',serviceNodeId:'service-manager',engine:'process',enabled:true,settings:{}});
    const selected=configurationBootstrapPlan(config,path.join(root,'config.json'),candidates[5]);
    const covering=configurationBootstrapPlan(config,path.join(root,'config.json'),candidates[4],undefined,[{candidate,manifest}]);
    const individual=configurationBootstrapPlan(config,path.join(root,'config.json'),candidate);
    const start=f.plans.length;f.plans.push(selected,covering,individual);
    for(const [n,plan]of[selected,covering,individual].entries()){await retainBootstrapPlan(plan);const time=new Date(old.getTime()+5000+n*1000);await utimes(path.join(config.runtimeRoot,'bootstrap-plans',hashJson(plan).slice(7)+'.json'),time,time);}
    await atomicJson(path.join(config.runtimeRoot,'bootstrap',selected.installationId+'.json'),selected);
    journal.db.prepare('INSERT INTO installed VALUES (?,?)').run('host-executor',JSON.stringify({candidateId:selected.candidateId}));
    journal.db.prepare('INSERT INTO runtime_targets VALUES (?,?)').run('x-ref',JSON.stringify({candidateId:candidates[4].candidateId}));
    journal.db.prepare('INSERT INTO runtime_targets VALUES (?,?)').run('y-ref',JSON.stringify({candidateId:candidate.candidateId}));
    assert.equal(start,6);
  }else if(input.case.startsWith('bootstrap')){
    await f.select(input.case==='bootstrap-prior'?3:2);
    if(input.case==='bootstrap-prior')await atomicJson(path.join(config.runtimeRoot,'bootstrap-maintenance',hashJson('retention-maintenance-fixture').slice(7),'record.json'),{phase:'needs_attention',updatedAt:new Date().toISOString(),request:makeRequest('retention-maintenance-fixture')});
  }else if(input.case==='maintenance-ring'){
    await f.select(3);let counter=0;
    for(let n=0;n<25;n++){
      const request=makeRequest('old-maintenance-'+n,n===0?f.plans[3]:f.plans[5]);const at=new Date(input.clock-60000+n*1000).toISOString();
      const record={schemaVersion:1,request,operationId:request.operationId,requestHash:hashJson(request),createdAt:at,updatedAt:at,phase:'succeeded',errorCode:null,definitions:[{instanceId:'host-executor',name:request.next.processes[0].name,previousHash:hashJson('prior'),desiredHash:hashJson('desired'),applied:true}]};
      const dir=path.join(config.runtimeRoot,'bootstrap-maintenance',hashJson(request.operationId).slice(7));await atomicJson(path.join(dir,'record.json'),record);await atomicJson(path.join(dir,'host-executor.prior.json'),{content:'prior'});await atomicJson(path.join(dir,'host-executor.desired.json'),{content:'desired'});counter++;
    }
    assert.equal(counter,25);
  }else assert.fail('Unknown retention fixture');
  const previous={schemaVersion:1,hostId:config.hostId,state:'succeeded',trigger:'manual',startedAt:'2000-01-01T00:00:00.000Z',completedAt:'2000-01-01T00:00:00.000Z',lastSuccessAt:'2000-01-01T00:00:00.000Z',removed:{candidates:0,preparations:0,payloads:0,snapshots:0,maintenance:0,bootstrapPlans:0},...await hostStorageSpace(config),skipped:[],errorCode:null};journal.saveStorageRetentionStatus(previous);
  const historyRecords={};const times=[];for(const name of await names(path.join(config.runtimeRoot,'bootstrap-maintenance'))){const p=path.join(config.runtimeRoot,'bootstrap-maintenance',name,'record.json');historyRecords[name]=contentsHash(p);const r=JSON.parse(readFileSync(p,'utf8'));if(typeof r.updatedAt==='string')times.push(r.updatedAt);else times.push('');}
  const setup={config,held,clock:input.clock,planKeys:(f.plans??[]).map((p,n)=>[hashJson(p).slice(7)+'.json','plan-'+n]),historyRecords,localeOrder:[...new Set(times)].sort((a,b)=>a.localeCompare(b)),fixtureSourceHash:'sha256:'+createHash('sha256').update(testSource).digest('hex')};
  journal.close();await writeFile(setupPath,JSON.stringify(setup));process.stdout.write(JSON.stringify(setup));
}else{
  const setup=JSON.parse(readFileSync(setupPath,'utf8')),config=setup.config;
  if(input.kind==='run'||input.kind==='run-sorted-control'){
    if(input.kind==='run-sorted-control'){
      assert.equal(input.case,'bootstrap-order');
      // Controlled emulation of the prior Rust sorted-set boundary. The
      // collector itself is the unmodified original implementation.
      const original=HostJournal.prototype.retentionCandidateIds;
      HostJournal.prototype.retentionCandidateIds=function(){return new Set([...original.call(this)].sort());};
    }
    const held=setup.held?new ExecutorLock(setup.held):null;
    const outcome=await wrap(()=>collectHostStorage(config,input.clock,'manual'));held?.close();process.stdout.write(JSON.stringify(outcome));
  }else if(input.kind==='observe'){
    const journal=new HostJournal(config);
    try{
      const details=[];for(const name of await names(path.join(config.runtimeRoot,'bootstrap-maintenance'))){const p=path.join(config.runtimeRoot,'bootstrap-maintenance',name);details.push({name,files:await names(p),recordUnchanged:contentsHash(path.join(p,'record.json'))===setup.historyRecords[name]});}
      const plans=new Map(setup.planKeys);
      const observed={status:journal.storageRetentionStatus(),registered:journal.db.prepare('SELECT candidate_id FROM candidates ORDER BY candidate_id').all().map(r=>r.candidate_id),pending:journal.pendingCandidateRetirements(),candidateDirectories:await names(path.join(config.artifactRoot,'candidates')),preparations:(await names(config.stagingRoot)).filter(n=>n.startsWith('build-')||n.startsWith('.retiring-build-')),snapshots:await names(path.join(config.stagingRoot,'sources')),plans:(await names(path.join(config.runtimeRoot,'bootstrap-plans'))).map(n=>plans.get(n)??n).sort(),details};
      if(input.case==='retired-link')observed.kept=await readFile(path.join(root,'kept','sentinel'),'utf8');
      process.stdout.write(JSON.stringify(observed));
    }finally{journal.close();}
  }else assert.fail('Unknown retention fixture operation');
}
