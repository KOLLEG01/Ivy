import assert from 'node:assert/strict';
import {readFileSync,existsSync} from 'node:fs';
import {mkdir,writeFile,readFile,rm,readdir,lstat} from 'node:fs/promises';
import {execFile} from 'node:child_process';
import {promisify} from 'node:util';
import {registerHooks,createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {DatabaseSync} from 'node:sqlite';
const input=JSON.parse(readFileSync(0,'utf8')),boundary=path.resolve(input.fixtureRoot);
assert(path.basename(boundary).startsWith('ivy-config-'));
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')){const url=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(url)))return next(url.href,context);}return next(specifier,context);}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {validateHost}=await import('../../packages/contracts/src/host-validation.ts');
const {atomicJson,jsonFile,instanceConfig}=await import('../../packages/host-runtime/src/config.ts');
const {HostJournal,ExecutorLock}=await import('../../packages/host-runtime/src/journal.ts');
const {runtimeResetMarker,hiveResetBootstrapMarker}=await import('../../packages/host-runtime/src/runtime-maintenance.ts');
const {servicePaths}=await import('../../packages/host-runtime/src/layout.ts');
const {stopContainer,startContainer}=await import('../../packages/host-runtime/src/docker.ts');
const {defaultSystemdAdapter,systemdUnitName}=await import('../../packages/host-runtime/src/systemd.ts');
const {installationIdentity}=await import('../../packages/host-runtime/src/linux-resources.ts');
const source=readFileSync(new URL('../../packages/host-runtime/src/runtime-reset.ts',import.meta.url),'utf8');
const ts=createRequire(import.meta.url)(process.env.IVY_TEST_TYPESCRIPT_MODULE??'typescript'),ast=ts.createSourceFile('reset.ts',source,ts.ScriptTarget.ESNext,true);
const names=['requireAdministrator','exists','sendStop','alive','waitStopped','proveLocksReleased','stopHost','savedRuntime','prepareLocal','completeLocal','confirmLocal','bootstrapLocal','releaseLocal'];
const functions=ast.statements.filter(n=>ts.isFunctionDeclaration(n)&&names.includes(n.name?.text)).map(n=>n.getText(ast));assert.equal(functions.length,names.length);
const variables=ast.statements.filter(n=>ts.isVariableStatement(n)&&n.declarationList.declarations.some(d=>['resetDirectory','recordPath'].includes(d.name.getText(ast)))).map(n=>n.getText(ast));assert.equal(variables.length,2);
const original=functions.join('\n'),admitted=original.replaceAll('  await requireAdministrator();\n','');assert.equal((original.match(/await requireAdministrator\(\);/g)??[]).length,5);
const NativeDate=Date,at='2026-10-20T14:00:00.000Z';
class FixtureDate extends NativeDate{constructor(...args){super(...(args.length?args:[at]));}static now(){return NativeDate.parse(at);}}
function implementation(full){
 const scope={requireThat,IvyError,execute:promisify(execFile),jsonFile,atomicJson,instanceConfig,hashJson,validateHost,HostJournal,ExecutorLock,runtimeResetMarker,hiveResetBootstrapMarker,servicePaths,stopContainer,startContainer,defaultSystemdAdapter,systemdUnitName,installationIdentity,mkdir,rm,lstat,join:path.join,dirname:path.dirname,Date:FixtureDate,process,setTimeout};
 const body=ts.transpileModule(variables.join('\n')+'\n'+(full?original:admitted)+'\nreturn {bootstrapLocal,releaseLocal,prepareLocal,completeLocal,confirmLocal};',{compilerOptions:{target:ts.ScriptTarget.ESNext}}).outputText;
 return new Function(...Object.keys(scope),body)(...Object.values(scope));
}
const sha=b=>'sha256:'+createHash('sha256').update(b).digest('hex'),hash=c=>'sha256:'+c.repeat(64),setupFile=path.join(boundary,'release-setup.json');
const safe=p=>{const target=path.resolve(p);assert(target.startsWith(boundary+path.sep));assert.notEqual(target,boundary);return target;};
async function file(p){try{const m=await lstat(p);return m.isDirectory()?{directory:true}:{base64:(await readFile(p)).toString('base64')};}catch(error){if(error.code==='ENOENT')return null;throw error;}}
async function reset(s){
 for(const p of [path.dirname(runtimeResetMarker(s.config)),s.runtime.dataRoot,...s.config.instances.map(i=>path.join(s.config.runtimeRoot,'owners',i.instanceId)),path.join(s.config.runtimeRoot,'executor')])await rm(safe(p),{recursive:true,force:true});
 const root=safe(path.dirname(runtimeResetMarker(s.config)));await mkdir(root,{recursive:true});
 if(s.recordDirectory)await mkdir(path.join(root,'current.json'));
 else if(s.recordBytes!==null)await writeFile(path.join(root,'current.json'),Buffer.from(s.recordBytes,'base64'));
 if(s.markerDirectory)await mkdir(runtimeResetMarker(s.config));
 else if(s.markerBytes!==null)await writeFile(runtimeResetMarker(s.config),Buffer.from(s.markerBytes,'base64'));
 if(s.journalDirectory){await rm(safe(path.join(s.config.runtimeRoot,'deployments.sqlite')),{force:true});await mkdir(path.join(s.config.runtimeRoot,'deployments.sqlite'));}
}
async function observe(s){
 const journalPath=path.join(s.config.runtimeRoot,'deployments.sqlite'),meta=await lstat(journalPath);let authority=null;
 if(meta.isFile()){
  const db=new DatabaseSync(journalPath,{readOnly:true});try{authority={candidates:db.prepare('SELECT candidate_json,manifest_json FROM candidates ORDER BY candidate_id').all(),targets:db.prepare('SELECT instance_id,value FROM runtime_targets ORDER BY instance_id').all(),installed:db.prepare('SELECT instance_id,value FROM installed ORDER BY instance_id').all()};}finally{db.close();}
 }
 const root=path.dirname(runtimeResetMarker(s.config)),members=await readdir(root);const locks=[];
 for(const name of ['executor',...s.config.instances.map(i=>'owners/'+i.instanceId)]){
  const p=path.join(s.config.runtimeRoot,name,'executor-lock.sqlite');if(!existsSync(p))continue;
  const db=new DatabaseSync(p,{readOnly:true});try{const owners=db.prepare('SELECT pid FROM owner').all();locks.push({name,count:owners.length,positivePid:owners.every(r=>r.pid>0)});}finally{db.close();}
 }
 return{record:await file(path.join(root,'current.json')),marker:await file(runtimeResetMarker(s.config)),hiveMarker:await file(hiveResetBootstrapMarker(s.runtime.dataRoot)),runtimeDataExists:existsSync(s.runtime.dataRoot),members:members.filter(n=>!n.endsWith('.tmp')).sort(),temporaryFiles:members.filter(n=>n.endsWith('.tmp')).length,authority,locks};
}
if(input.kind==='setup'){
 const config=input.config,which=input.case,group=input.group;config.ivyRoot=path.join(boundary,'installation');
 const instance=config.instances[0];
 if(which==='hive-unicode-target')instance.instanceId='fixture-é🚀';
 if(which==='hive-surrogate-target')instance.instanceId='fixture-'+String.fromCharCode(0xd800);
 if(which.startsWith('hive-')||['disabled-hive','process-hive'].includes(which)){instance.componentId='hive';instance.engine=which==='process-hive'?'process':'docker';}
 else if(which.startsWith('docker-'))instance.engine='docker';
 if(which==='disabled-config')instance.enabled=false;
 validateHost('HostConfig',config);
 const runtime={schemaVersion:1,hostId:config.hostId,instanceId:instance.instanceId,serviceNodeId:instance.serviceNodeId,componentId:instance.componentId,publicBaseUrl:'https://example.invalid',dataRoot:path.join(boundary,'runtime-data'),artifactRoot:path.join(boundary,'artifact'),buildId:hash('a'),version:'1.0.0',settings:{}};
 validateHost('InstanceConfig',runtime);
 const candidate={candidateId:hash('a'),componentId:instance.componentId,artifactRoot:runtime.artifactRoot,manifestPath:path.join(runtime.artifactRoot,'manifest.json'),createdAt:at,platform:{os:process.platform,arch:process.arch,node:process.versions.node},buildId:hash('a')};
 const manifest={schemaVersion:1,componentId:instance.componentId,kind:'app',version:'1.0.0',buildId:hash('a'),connectsToHive:false,requirements:{node:'>=24.18.0 <25.0.0',hiveProtocol:null,contracts:[]},shutdown:{timeoutMs:1000},app:{appId:'fixture',dist:'dist/apps/fixture',entryPath:'index.html'}};
 validateHost('Candidate',candidate);validateHost('ReleaseManifest',manifest);
 const runtimePath=path.join(config.runtimeRoot,'configs','fixture.json');await mkdir(path.dirname(runtimePath),{recursive:true});await mkdir(path.join(runtime.artifactRoot,'dist'),{recursive:true});
 if(!which.endsWith('missing-runtime'))await writeFile(runtimePath,which.endsWith('invalid-runtime')?'{invalid':JSON.stringify(runtime));
 await writeFile(path.join(runtime.artifactRoot,'dist','build-info.json'),JSON.stringify({buildId:which.endsWith('build-mismatch')?hash('b'):runtime.buildId,version:runtime.version}));
 const journal=new HostJournal(config);
 try{
  journal.saveCandidate(candidate,manifest);
  if(which.startsWith('hive-')||which.startsWith('docker-')){
   if(!which.endsWith('missing-target'))journal.restoreTarget({schemaVersion:1,instanceId:instance.instanceId,revision:'fixture-launch',candidateId:candidate.candidateId,desired:which.endsWith('stopped-target')?'stopped':'running',configPath:runtimePath,requestedAt:at});
  }
 }finally{journal.close();}
 const args={resetId:'fixture-reset',runtimeEpoch:'fixture-epoch',publisherDigest:hash('d')};
 const plan={schemaVersion:1,deploymentIdentity:'fixture-deployment',hostId:config.hostId,configPath:path.join(boundary,'host.json'),instances:config.instances.map(i=>({instanceId:i.instanceId,componentId:i.componentId,enabled:i.enabled})),scopeRootIds:[],delete:[],preserve:[],completionMarkers:[]};
 let record={schemaVersion:1,deploymentIdentity:plan.deploymentIdentity,resetId:args.resetId,runtimeEpoch:args.runtimeEpoch,hostId:config.hostId,phase:'complete',updatedAt:'2026-01-01T00:00:00.000Z',planHash:hashJson(plan),plan,saved:[{instanceId:instance.instanceId,enabled:true,candidate,manifest}],instructions:[{retained:'instruction authority'}],publisherCredentialDigest:args.publisherDigest,instructionsCaptured:true,deploymentComplete:false,retainedExtra:{originalOperationId:'fixture-original'}};
 if(which==='no-enabled')record.saved=[];
 if(which==='disabled-hive')record.saved[0].enabled=false;
 if(which==='docker-truthy-enabled')record.saved[0].enabled=[];
 if(which==='wrong-reset')args.resetId='different';if(which==='wrong-epoch')args.runtimeEpoch='different';
 if(['prepared','cleared','released','already-released','released-bad-saved'].includes(which))record.phase=which.includes('released')?'released':which;
 if(which==='saved-null'||which==='released-bad-saved')record.saved=null;
 if(which==='saved-entry-null')record.saved=[null];
 if(which==='deployment-complete')record.deploymentComplete=true;
 let recordBytes=Buffer.from(JSON.stringify(record,null,2)+'\n').toString('base64');
 if(which==='null-record')recordBytes=Buffer.from('null').toString('base64');
 if(which==='malformed-record')recordBytes=Buffer.from('{invalid').toString('base64');
 if(which==='missing-record')recordBytes=null;
 const s={config,runtime,record,plan,args,group,case:which,at,recordBytes,markerBytes:which==='missing-hold'?null:Buffer.from('retained hold\n').toString('base64'),markerDirectory:which==='hold-directory'||which==='marker-directory',recordDirectory:false,journalDirectory:which==='journal-directory'};
 await reset(s);await writeFile(setupFile,JSON.stringify(s));process.stdout.write(JSON.stringify(s));
}else{
 const s=JSON.parse(await readFile(setupFile,'utf8'));
 if(input.kind==='reset'){await reset(s);process.stdout.write(JSON.stringify({reset:true}));}
 else if(input.kind==='observe')process.stdout.write(JSON.stringify(await observe(s)));
 else if(input.kind==='run'){
  const full=input.full??false,impl=implementation(full);let outcome;
  try{
   let value;
   if(s.group==='bootstrap')value=await impl.bootstrapLocal(s.config,s.args.resetId,s.args.runtimeEpoch);
   else if(s.group==='release')value=await impl.releaseLocal(s.config,s.args.resetId,s.args.runtimeEpoch);
   else if(s.group==='resume'){
    const first=await impl.releaseLocal(s.config,s.args.resetId,s.args.runtimeEpoch);
    const held=await impl.prepareLocal(s.config,s.plan,s.args.resetId,s.args.runtimeEpoch,s.args.publisherDigest);
    const completed=await impl.completeLocal(s.config,s.args.resetId,s.args.runtimeEpoch);
    const released=await impl.releaseLocal(s.config,s.args.resetId,s.args.runtimeEpoch);
    const confirmed=await impl.confirmLocal(s.config,s.args.resetId,s.args.runtimeEpoch);
    value={first,held,completed,released,confirmed};
   }else assert.fail('Unknown phase fixture');
   outcome={ok:true,value};
  }catch(error){outcome={ok:false,error:IvyError.from(error).toWire()};}
  process.stdout.write(JSON.stringify({outcome,state:await observe(s),sourceHash:sha(source),declarationHash:sha(variables.join('\n')+original),admittedCutHash:sha(admitted),excludedStatements:full?[]:['requireAdministrator statements in prepareLocal,completeLocal,confirmLocal,bootstrapLocal,releaseLocal'],node:{executable:process.execPath,version:process.versions.node}}));
 }else assert.fail('Unknown release fixture operation');
}
