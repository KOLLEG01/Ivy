import assert from 'node:assert/strict';
import {readFileSync,existsSync} from 'node:fs';
import {mkdir,writeFile,readFile,readdir,copyFile,rm,lstat,readlink,symlink} from 'node:fs/promises';
import {registerHooks,createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {DatabaseSync} from 'node:sqlite';
const input=JSON.parse(readFileSync(0,'utf8')),boundary=path.resolve(input.fixtureRoot),root=path.resolve(input.root);
assert(path.basename(boundary).startsWith('ivy-config-'));assert.equal(path.dirname(root),boundary);
const target=path.join(root,'target'),setupPath=path.join(root,'publication-test-setup.json');
const sourceUrl=new URL('../../packages/host-runtime/src/restore.ts',import.meta.url),source=readFileSync(sourceUrl,'utf8');
const ts=createRequire(import.meta.url)(process.env.IVY_TEST_TYPESCRIPT_MODULE??'typescript');
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')){const url=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(url)))return next(url.href,context);}return next(specifier,context);}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {hashJson,canonical}=await import('../../packages/contracts/src/canonical.ts');
const {validateHost}=await import('../../packages/contracts/src/host-validation.ts');
const {HostJournal,terminalPhases}=await import('../../packages/host-runtime/src/journal.ts');
const {installationRecord}=await import('../../packages/host-runtime/src/accepted-configuration.ts');
const {inventoryFiles,exists,copyVerified,privateDirectory,relativeFile,syncDirectories}=await import('../../packages/host-runtime/src/backup-files.ts');
const {hostStorageAreas,servicePaths,hostConfigurationPath,storageFile}=await import('../../packages/host-runtime/src/layout.ts');
const {restoredStorageAreas}=await import('../../packages/host-runtime/src/project-storage.ts');
const {requireStorageSpace}=await import('../../packages/host-runtime/src/storage-space.ts');
const {nativeBackupExclusions,sharedNativeHomes}=await import('../../packages/host-runtime/src/native-backup-paths.ts');
const {atomicJson}=await import('../../packages/host-runtime/src/config.ts');
const sha=b=>'sha256:'+createHash('sha256').update(b).digest('hex'),at='2026-10-20T00:00:00.000Z',hash=c=>'sha256:'+c.repeat(64);
const wrap=async fn=>{try{return{ok:true,value:(await fn())??null};}catch(error){return{ok:false,error:IvyError.from(error).toWire()};}};
const copyStart=source.indexOf('  const areas = restoredStorageAreas(config,'),copyEnd=source.indexOf('  const maintenanceOriginalBytes');
const platformStart=source.indexOf('  requireThat(manifest.platform.os'),platformEnd=source.indexOf('  const restoreId =',platformStart);
const finishStart=source.indexOf('  const db = new DatabaseSync('),finishEnd=source.indexOf('\n}',finishStart);
assert(copyStart>=0&&copyEnd>copyStart&&finishEnd>finishStart);
const compile=(parameters,body)=>new (Object.getPrototypeOf(async function(){}).constructor)(...parameters,ts.transpileModule(body,{compilerOptions:{target:ts.ScriptTarget.ESNext}}).outputText);
const platformCheck=compile(['manifest','requireThat'],source.slice(platformStart,platformEnd));
const originalCopy=compile(['manifest','config','backupRoot','restoredStorageAreas','dirname','exists','requireStorageSpace','privateDirectory','hostConfigurationPath','mkdir','copyVerified','relativeFile','storageFile','join'],source.slice(copyStart,copyEnd)+'\nreturn total;');
const copy=setup=>originalCopy(setup.manifest,setup.config,setup.backupRoot,restoredStorageAreas,path.dirname,exists,requireStorageSpace,privateDirectory,hostConfigurationPath,mkdir,copyVerified,relativeFile,storageFile,path.join);
const finish=compile(['config','createdAt','DatabaseSync','join','basename','terminalPhases','validateHost','canonical','installationRecord','requireThat','servicePaths','exists','unlink','hostStorageAreas','syncDirectories','atomicJson','restoreId','manifest','manifestHash','configPath','total','expandedBytes','maintenanceOriginalBytes','nativeRecovery','started'],source.slice(finishStart,finishEnd));
async function observe(setup){
 const files=[];
 const scan=async(dir,prefix)=>{if(!await exists(dir))return;for(const name of(await readdir(dir)).sort()){const full=path.join(dir,name),local=prefix+name,m=await lstat(full);if(m.isSymbolicLink())files.push({path:local,link:await readlink(full)});else if(m.isDirectory())await scan(full,local+'/');else if(local==='state/deployments.sqlite'){
 const db=new DatabaseSync(full,{readOnly:true});try{const tables={};for(const table of ['meta','operations','candidates','installed','observations','runtime_targets','candidate_retirements']){if(!db.prepare("SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?").get(table))continue;tables[table]=db.prepare('SELECT * FROM '+table).all().map(row=>Object.fromEntries(Object.entries(row).map(([k,v])=>{if(typeof v==='string')try{v=JSON.parse(v);}catch{}return[k,v];}))).sort((a,b)=>canonical(a)<canonical(b)?-1:canonical(a)>canonical(b)?1:0);}files.push({path:local,tables,quickCheck:db.prepare('PRAGMA quick_check').get().quick_check});}finally{db.close();}
 }else files.push({path:local,bytes:m.size,hash:sha(await readFile(full))});}};
 for(const area of hostStorageAreas(setup.config))await scan(area.path,area.key+'/');
 const published=hostConfigurationPath(setup.config);return{files,publication:await exists(published)?(await lstat(published)).isFile()?JSON.parse(await readFile(published,'utf8')):'directory':null,sourceJournalHash:sha(await readFile(setup.sourceJournal)),backupFiles:await inventoryFiles(setup.backupRoot),outside:await exists(path.join(root,'outside-control'))?await readFile(path.join(root,'outside-control'),'utf8'):null};
}
async function prepareWorking(setup){
 await copy(setup);
 const data=servicePaths(setup.config,setup.config.instances[0]).data;await mkdir(data,{recursive:true});
 for(const name of ['health.json','control.json'])await writeFile(path.join(data,name),JSON.stringify({launchId:'captured-launch',name}));
 await writeFile(path.join(data,'kept.json'),'retained service payload');
 if(input.case==='health-directory'){await rm(path.join(data,'health.json'));await mkdir(path.join(data,'health.json'));}
 if(input.case==='control-link'){await rm(path.join(data,'control.json'));await writeFile(path.join(root,'outside-control'),'external evidence');await symlink(path.join(root,'outside-control'),path.join(data,'control.json'),'file');}
 if(input.case==='config-publication-failure')await mkdir(hostConfigurationPath(setup.config));
 if(input.case==='wrong-journal-host'){const db=new DatabaseSync(path.join(setup.config.runtimeRoot,'deployments.sqlite'));try{db.prepare("UPDATE meta SET value='different' WHERE key='hostId'").run();}finally{db.close();}}
}
if(input.kind==='setup'){
 await mkdir(target,{recursive:true});
 const original={schemaVersion:1,hostId:'restore-fixture',runtimeRoot:path.join(root,'source-state'),artifactRoot:path.join(root,'source-artifacts'),stagingRoot:path.join(root,'source-staging'),publicBaseUrl:'http://127.0.0.1:38081/ivy',executables:{node:process.execPath},instances:[{instanceId:'fixture',componentId:'fixture',serviceNodeId:'fixture-node',engine:'process',enabled:true,settings:{}}]};
 if(input.case==='external-area')original.instances[0].paths={data:path.join(root,'source-data'),work:path.join(original.runtimeRoot,'instances','fixture','work'),logs:path.join(original.runtimeRoot,'instances','fixture','logs')};
 const journal=new HostJournal(original);
 try{
  for(const [n,phase]of ['preparing','succeeded'].entries()){
   const id='deployment-'+n,entry={schemaVersion:1,request:{action:'restart',operationId:'restore-op-'+n,instanceId:'fixture'},record:{deploymentId:id,hostId:original.hostId,instanceId:'fixture',componentId:'fixture',requestHash:hash('a'),previousBuild:null,targetBuild:hash('b'),observedBuild:null,phase,createdAt:at,updatedAt:at,readiness:{state:'not_run',message:''}},candidateId:null,previousCandidateId:null,configurationPath:path.join(root,'old-config','accepted-'+n+'.json'),activation:{previousTarget:null,previousEnabled:true,targetEnabled:true,rollbackTargetRevision:null,previousConfigurationPath:path.join(root,'old-config','prior-'+n+'.json')}};
   validateHost('JournalEntry',entry);journal.db.prepare('INSERT INTO operations VALUES (?,?,?,?,?,?)').run(entry.request.operationId,id,entry.record.requestHash,phase,at,canonical(entry));
  }
 }finally{journal.close();}
 const sourceJournal=path.join(original.runtimeRoot,'deployments.sqlite'),backupRoot=path.join(root,'backup');await mkdir(path.join(backupRoot,'state'),{recursive:true});await copyFile(sourceJournal,path.join(backupRoot,'state','deployments.sqlite'));await atomicJson(path.join(backupRoot,'configuration.json'),original);
 for(const member of ['restore.json','host.json','native-recovery.json','project-recovery.json','restore-decisions/prior.json','recovery-original/prior.txt','previous-recovery/prior.txt','accepted-configurations/accepted.json','payload.txt']){const file=path.join(backupRoot,'state',member);await mkdir(path.dirname(file),{recursive:true});await writeFile(file,'captured '+member);}
 const captured=hostStorageAreas(original);
 if(input.case==='external-area'){await mkdir(path.join(backupRoot,'services','fixture','data'),{recursive:true});await writeFile(path.join(backupRoot,'services','fixture','data','payload.txt'),'separate owned storage');}
 await mkdir(path.join(backupRoot,'ignored'),{recursive:true});await writeFile(path.join(backupRoot,'ignored','not-storage'),'not a mapped storage member');
 if(input.case==='missing-journal')await rm(path.join(backupRoot,'state','deployments.sqlite'));
 const config={...original,runtimeRoot:path.join(target,'state'),artifactRoot:path.join(target,'artifacts'),stagingRoot:path.join(target,'staging'),configPath:path.join(target,'restored-host.json'),publicBaseUrl:'http://127.0.0.1:38082/ivy',instances:original.instances.map(i=>({...i,enabled:false,...(i.paths?{paths:{data:path.join(target,'external-data'),work:path.join(target,'state','instances','fixture','work'),logs:path.join(target,'state','instances','fixture','logs')}}:{} )})),restoredFrom:{backupId:'00000000-0000-4000-8000-000000000086',manifestHash:hash('a'),restoreId:'00000000-0000-4000-8000-000000000087'}};
 const manifest={schemaVersion:1,backupId:config.restoredFrom.backupId,hostId:config.hostId,createdAt:at,completedAt:at,consistency:'offline-owned-state',platform:{os:process.platform,arch:process.arch,node:process.version},configurationHash:hashJson(original),files:await inventoryFiles(backupRoot),externalFiles:[],hiveSnapshots:[],storageAreas:captured,excluded:[]};
 if(input.case==='legacy-areas')delete manifest.storageAreas;
 if(input.case==='changed-source')await writeFile(path.join(backupRoot,'state','payload.txt'),'changed captured bytes');
 if(input.case==='existing-root'){await mkdir(config.runtimeRoot);await writeFile(path.join(config.runtimeRoot,'owner.txt'),'preexisting authority');}
 validateHost('HostConfig',config);validateHost('BackupManifest',manifest);
 if(input.case==='wrong-os')manifest.platform.os=process.platform==='win32'?'linux':'win32';
 if(input.case==='wrong-arch')manifest.platform.arch=process.arch==='x64'?'arm64':'x64';
 if(input.case==='wrong-node')manifest.platform.node='v24.17.0';
 if(input.case==='missing-node-prefix')manifest.platform.node=process.version.slice(1);
 const setup={config,original,backupRoot,manifest,sourceJournal,at,exclusions:nativeBackupExclusions(original),shared:sharedNativeHomes(config),localeOrder:['fixture'],sourceHash:sha(source),copySpanHash:sha(source.slice(copyStart,copyEnd)),publicationSpanHash:sha(source.slice(finishStart,finishEnd)),maintenanceOriginalBytes:31,nativeRecovery:{instances:[]}};
 if(input.group==='publication'){
  try{await prepareWorking(setup);}catch(error){
   if(input.case!=='control-link'||error.syscall!=='symlink'||!['EPERM','ENOTSUP'].includes(error.code))throw error;
   process.stdout.write(JSON.stringify({unavailable:{stage:'file-symlink fixture',code:error.code,syscall:error.syscall,message:error.message}}));process.exit(0);
  }
  const {relocateNativeStorage}=await import('../../packages/host-runtime/src/native-restore.ts');
  setup.nativeRecovery=await relocateNativeStorage(original,config,manifest,root);
  if(input.case==='report-accounting')setup.nativeRecovery.instances=[{originalFiles:[{bytes:17},{bytes:5}]}];
  await atomicJson(path.join(config.runtimeRoot,'native-recovery.json'),setup.nativeRecovery);
 }
 await writeFile(setupPath,JSON.stringify(setup));process.stdout.write(JSON.stringify(setup));
}else{
 const setup=JSON.parse(await readFile(setupPath,'utf8'));
 if(input.kind==='observe')process.stdout.write(JSON.stringify(await observe(setup)));
 else if(input.kind==='reset'){
  const resolved=path.resolve(target);assert(resolved.startsWith(root+path.sep));assert.notEqual(resolved,root);assert.equal(path.dirname(resolved),root);
  await rm(resolved,{recursive:true,force:true});await mkdir(resolved);
  if(input.group==='publication'){await prepareWorking(setup);await atomicJson(path.join(setup.config.runtimeRoot,'native-recovery.json'),setup.nativeRecovery);}
  if(input.case==='existing-root'){await mkdir(setup.config.runtimeRoot);await writeFile(path.join(setup.config.runtimeRoot,'owner.txt'),'preexisting authority');}
  process.stdout.write(JSON.stringify({reset:true}));
 }else if(input.kind==='run'){
  const result=await wrap(async()=>{
   if(input.group==='payload')return copy(setup);
   if(input.group==='platform')return platformCheck(setup.manifest,requireThat);
   const {unlink}=await import('node:fs/promises');
   const value=await finish(setup.config,at,DatabaseSync,path.join,path.basename,terminalPhases,validateHost,canonical,installationRecord,requireThat,servicePaths,exists,unlink,hostStorageAreas,syncDirectories,atomicJson,setup.config.restoredFrom.restoreId,setup.manifest,setup.config.restoredFrom.manifestHash,hostConfigurationPath(setup.config),setup.manifest.files.reduce((n,f)=>n+f.bytes,0),0,setup.maintenanceOriginalBytes,setup.nativeRecovery,performance.now());
   delete value.elapsedMs;return value;
  });process.stdout.write(JSON.stringify(result));
 }else assert.fail('Unknown publication fixture operation');
}
