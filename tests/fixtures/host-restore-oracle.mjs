import assert from 'node:assert/strict';
import {readFileSync,existsSync} from 'node:fs';
import {mkdir,writeFile,readFile,readdir,copyFile,symlink} from 'node:fs/promises';
import {registerHooks,createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {DatabaseSync} from 'node:sqlite';
const input=JSON.parse(readFileSync(0,'utf8')),boundary=path.resolve(input.fixtureRoot),root=path.resolve(input.root);
assert(path.basename(boundary).startsWith('ivy-config-'));assert.equal(path.dirname(root),boundary);
const ts=createRequire(import.meta.url)(process.env.IVY_TEST_TYPESCRIPT_MODULE??'typescript');
const restoreUrl=new URL('../../packages/host-runtime/src/restore.ts',import.meta.url);
const source=readFileSync(restoreUrl,'utf8');
const sha=b=>'sha256:'+createHash('sha256').update(b).digest('hex');
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')){const url=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(url)))return next(url.href,context);}return next(specifier,context);},load(url,context,next){if(url===restoreUrl.href)return{format:'module',shortCircuit:true,source:ts.transpileModule(source+'\nexport { targetConfiguration };',{compilerOptions:{target:ts.ScriptTarget.ESNext,module:ts.ModuleKind.ESNext}}).outputText};return next(url,context);}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {hashJson,canonical}=await import('../../packages/contracts/src/canonical.ts');
const {validateHost}=await import('../../packages/contracts/src/host-validation.ts');
const {HostJournal,terminalPhases}=await import('../../packages/host-runtime/src/journal.ts');
const {installationRecord}=await import('../../packages/host-runtime/src/accepted-configuration.ts');
const {inventoryFiles,exists}=await import('../../packages/host-runtime/src/backup-files.ts');
const {hostStorageAreas,servicePaths,resolveHostConfiguration,hostConfigurationPath}=await import('../../packages/host-runtime/src/layout.ts');
const {nativeBackupExclusions,sharedNativeHomes}=await import('../../packages/host-runtime/src/native-backup-paths.ts');
const {atomicJson}=await import('../../packages/host-runtime/src/config.ts');
const {defaultServiceSettings}=await import('../../packages/host-runtime/src/default-settings.ts');
const wrap=async fn=>{try{return{ok:true,value:(await fn())??null};}catch(error){return{ok:false,error:IvyError.from(error).toWire()};}};
const setupPath=path.join(root,'restore-test-setup.json'),at='2026-10-20T00:00:00.000Z';
const hash=c=>'sha256:'+c.repeat(64);
const tables=config=>{const db=new DatabaseSync(path.join(config.runtimeRoot,'deployments.sqlite'),{readOnly:true});try{const result={};for(const name of ['meta','operations','candidates','installed','observations','runtime_targets','candidate_retirements']){if(!db.prepare("SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?").get(name))continue;result[name]=db.prepare('SELECT * FROM '+name).all().map(row=>Object.fromEntries(Object.entries(row).map(([k,v])=>{if(typeof v==='string'){try{v=JSON.parse(v);}catch{}}return[k,v];})));result[name].sort((a,b)=>canonical(a)<canonical(b)?-1:canonical(a)>canonical(b)?1:0);}return{tables:result,quickCheck:db.prepare('PRAGMA quick_check').get().quick_check};}finally{db.close();}};
if(input.kind==='setup'){
 await mkdir(root,{recursive:true});
 const config={schemaVersion:1,hostId:'restore-fixture',runtimeRoot:path.join(root,'source-state'),artifactRoot:path.join(root,'source-artifacts'),stagingRoot:path.join(root,'source-staging'),publicBaseUrl:'http://127.0.0.1:38081/ivy',executables:{node:process.execPath},instances:[{instanceId:'fixture',componentId:'fixture',serviceNodeId:'fixture-node',engine:'process',enabled:true,settings:{}}]};
 const backupRoot=path.join(root,'backup'),templatePath=path.join(root,'template.json');
 let captured=[],original=config,manifest=null,manifestHash=null,before=null;
 if(input.group==='verify'||input.group==='target'){
  if(input.case.startsWith('shared')||input.case.startsWith('retained')||input.case.startsWith('ephemeral')||input.case==='owned-native'){
   config.instances.push({instanceId:'agent',componentId:'agent-manager',serviceNodeId:'agent-node',engine:'process',enabled:true,settings:{appServer:{mode:'owned-stdio'},codexHome:path.join(config.runtimeRoot,'instances','agent','data','native'),internalProjectRoot:path.join(root,'source-projects')}});
   if(input.case.startsWith('shared')){const a=config.instances[1];a.settings.codexHome=path.join(root,'shared-home');a.settings.appServer={mode:'external-proxy',expectedCodexHome:a.settings.codexHome};}
  }
  if(input.case.startsWith('manager'))config.instances.push({instanceId:'manager',componentId:'host-executor',serviceNodeId:'manager-node',engine:'process',enabled:true,settings:{hostConfigPath:hostConfigurationPath(config)}});
  if(input.case.startsWith('hive'))config.instances.push({instanceId:'hive',componentId:'hive',serviceNodeId:'hive-node',engine:'process',enabled:true,settings:defaultServiceSettings({componentId:'hive',settings:{credentials:[]}},{ivyRoot:root,configPath:path.join(root,'original.json'),join:path.join})});
  validateHost('HostConfig',config);await mkdir(backupRoot,{recursive:true});await mkdir(path.join(backupRoot,'state'),{recursive:true});
  await atomicJson(path.join(backupRoot,'configuration.json'),config);await writeFile(path.join(backupRoot,'state','payload'),'immutable payload');
  captured=hostStorageAreas(config);
  if(input.case.startsWith('retained')){const area={path:path.join(root,'retained-project'),key:''};area.key='retained-projects/agent/'+hashJson(area.path).slice(7);captured.push(area);}
  manifest={schemaVersion:1,backupId:'00000000-0000-4000-8000-000000000086',hostId:config.hostId,createdAt:at,completedAt:at,consistency:'offline-owned-state',platform:{os:process.platform,arch:process.arch,node:process.version},configurationHash:hashJson(config),files:[],externalFiles:[],hiveSnapshots:[],ephemeralNativePaths:nativeBackupExclusions(config),sharedNativeHomes:sharedNativeHomes(config),storageAreas:captured,excluded:[]};
  if(input.group==='verify'){
   if(input.case==='ephemeral-contained'){const entry=manifest.ephemeralNativePaths[0];assert(entry);const member=(entry.path.startsWith('services/')?'':'state/')+entry.path;await mkdir(path.dirname(path.join(backupRoot,member)),{recursive:true});await writeFile(path.join(backupRoot,member),'excluded native cache');}
   manifest.files=await inventoryFiles(backupRoot);
   if(input.case==='duplicate'||input.case==='case-collision'){const extra={...manifest.files[0]};if(input.case==='case-collision')extra.path=extra.path.toUpperCase();manifest.files.push(extra);}
   if(input.case==='traversal')manifest.files[0].path='../unsafe';
   if(input.case==='configuration-hash')manifest.configurationHash=hash('f');
   if(input.case==='configuration-host')manifest.hostId='other-host';
   if(input.case==='captured-layout')manifest.storageAreas=[{key:'state',path:path.join(root,'wrong-state')}];
   if(input.case==='retained-overlap'){const area=manifest.storageAreas.at(-1);area.path=config.runtimeRoot;area.key='retained-projects/agent/'+hashJson(area.path).slice(7);}
   if(input.case==='shared-mismatch')manifest.sharedNativeHomes[0].path=path.join(root,'other-home');
   if(input.case==='shared-missing')manifest.sharedNativeHomes=[];
   if(input.case==='ephemeral-mismatch')manifest.ephemeralNativePaths=[];
   validateHost('BackupManifest',manifest);await atomicJson(path.join(backupRoot,'backup.json'),manifest);manifestHash=sha(readFileSync(path.join(backupRoot,'backup.json')));
   if(input.case==='hash-mismatch')manifestHash=hash('e');
   if(input.case==='failure-marker')await writeFile(path.join(backupRoot,'backup-failure.json'),'failure');
   if(input.case==='payload-changed')await writeFile(path.join(backupRoot,'state','payload'),'changed payload');
   if(input.case==='payload-missing')await (await import('node:fs/promises')).unlink(path.join(backupRoot,'state','payload'));
   if(input.case==='payload-extra')await writeFile(path.join(backupRoot,'extra'),'unexpected');
  }else{
   const target=path.join(root,'target');await mkdir(target);const raw=structuredClone(config);
   raw.runtimeRoot=path.join(target,'state');raw.artifactRoot=path.join(target,'artifacts');raw.stagingRoot=path.join(target,'staging');raw.publicBaseUrl='http://127.0.0.1:38082/ivy';
   for(const instance of raw.instances){instance.enabled=false;if(instance.componentId==='agent-manager'){if(!input.case.startsWith('shared'))instance.settings.codexHome=path.join(servicePaths(raw,instance).data,'native');instance.settings.internalProjectRoot=path.join(target,'projects');}if(instance.componentId==='host-executor')instance.settings.hostConfigPath=hostConfigurationPath(raw);if(instance.componentId==='hive'){instance.settings.listenPort=38082;instance.settings.backup.directory=path.join(raw.runtimeRoot,'hive-backups');}}
   if(input.case==='enabled')raw.instances[0].enabled=true;
   if(input.case==='host-identity')raw.hostId='other-host';
   if(input.case==='instance-identity')raw.instances[0].serviceNodeId='other-node';
   if(input.case==='same-endpoint')raw.publicBaseUrl=config.publicBaseUrl;
   if(input.case==='executables')raw.executables.node=path.join(root,'other-node');
   if(input.case.startsWith('prerequisite')){const p=path.join(root,'runtime-prerequisite');await writeFile(p,'recorded executable');manifest.externalFiles=[{path:p,hash:input.case==='prerequisite-changed'?hash('d'):sha(readFileSync(p)),bytes:19,mode:0o600}];}
   if(input.case==='existing-root')await mkdir(raw.runtimeRoot);
   if(input.case==='missing-parent')raw.stagingRoot=path.join(root,'absent','staging');
   if(input.case==='source-overlap'){await mkdir(config.runtimeRoot,{recursive:true});raw.runtimeRoot=path.join(config.runtimeRoot,'nested');}
   if(input.case==='linked-parent-overlap'){await mkdir(config.runtimeRoot,{recursive:true});const link=path.join(target,'linked');await symlink(config.runtimeRoot,link,process.platform==='win32'?'junction':'dir');raw.runtimeRoot=path.join(link,'nested');}
   if(input.case==='root-overlap')raw.stagingRoot=path.join(raw.artifactRoot,'staging');
   if(input.case==='original-config')raw.configPath=hostConfigurationPath(config);
   if(input.case==='config-in-artifacts')raw.configPath=path.join(raw.artifactRoot,'host.json');
   if(input.case==='config-exists'){raw.configPath=path.join(target,'host.json');await writeFile(raw.configPath,'retained');}
   if(input.case==='template-is-config')raw.configPath=templatePath;
   if(input.case==='manager-wrong-config')raw.instances.at(-1).settings.hostConfigPath=hostConfigurationPath(config);
   if(input.case==='hive-same-port')raw.instances.at(-1).settings.listenPort=config.instances.at(-1).settings.listenPort;
   if(input.case==='hive-backup-in-data')raw.instances.at(-1).settings.backup.directory=servicePaths(raw,raw.instances.at(-1)).data;
   if(input.case==='shared-changed'){raw.instances[1].settings.codexHome=path.join(root,'changed-shared');raw.instances[1].settings.appServer.expectedCodexHome=raw.instances[1].settings.codexHome;}
   await atomicJson(templatePath,raw);
   manifestHash=hash('a');
   const resolved=resolveHostConfiguration(raw,templatePath);before={roots:[resolved.runtimeRoot,resolved.artifactRoot,resolved.stagingRoot],present:await Promise.all([resolved.runtimeRoot,resolved.artifactRoot,resolved.stagingRoot].map(exists))};
   const setup={config:resolved,original,backupRoot,templatePath,manifest,manifestHash,restoreId:'00000000-0000-4000-8000-000000000087',shared:sharedNativeHomes(resolved),exclusions:nativeBackupExclusions(config),localeOrder:resolved.instances.map(i=>i.instanceId).sort((a,b)=>a.localeCompare(b)),sourceHash:sha(source),before};
   await writeFile(setupPath,JSON.stringify(setup));process.stdout.write(JSON.stringify(setup));process.exit(0);
  }
 }else if(input.group==='journal'){
  const journal=new HostJournal(config),db=journal.db;
  for(const[n,phase]of['preparing','succeeded','rolled_back','failed','needs_attention'].entries()){
   const id='deployment-'+n,entry={schemaVersion:1,request:{action:'restart',operationId:'restore-op-'+n,instanceId:'fixture'},record:{deploymentId:id,hostId:config.hostId,instanceId:'fixture',componentId:'fixture',requestHash:hash('a'),previousBuild:null,targetBuild:hash('b'),observedBuild:null,phase,createdAt:at,updatedAt:at,readiness:{state:'not_run',message:''}},candidateId:null,previousCandidateId:null,configurationPath:path.join(root,'old-config','accepted-'+n+'.json'),activation:{previousTarget:null,previousEnabled:true,targetEnabled:true,rollbackTargetRevision:null,previousConfigurationPath:path.join(root,'old-config','prior-'+n+'.json')}};
   validateHost('JournalEntry',entry);db.prepare('INSERT INTO operations VALUES (?,?,?,?,?,?)').run(entry.request.operationId,id,entry.record.requestHash,phase,at,canonical(entry));
  }
  for(const table of['candidates','installed','observations','runtime_targets','candidate_retirements']){if(table==='candidates')db.prepare('INSERT INTO candidates VALUES (?,?,?,?,?)').run(hash('b'),'fixture',hash('c'),'{}','{}');else if(table==='candidate_retirements')db.prepare('INSERT INTO candidate_retirements VALUES (?,?)').run(hash('d'),'{}');else db.prepare('INSERT INTO '+table+' VALUES (?,?)').run('fixture','{}');}
  for(const key of['storage-retention','bootstrap-rollback:fixture','unrelated-evidence'])db.prepare('INSERT INTO meta VALUES (?,?)').run(key,'preserved');
  if(input.case==='wrong-host')db.prepare("UPDATE meta SET value='other' WHERE key='hostId'").run();
  if(input.case==='wrong-version')db.exec('PRAGMA user_version=2');
  if(input.case==='malformed-entry')db.prepare("UPDATE operations SET entry_json='{}' WHERE deployment_id='deployment-2'").run();
  if(input.case==='legacy-no-retirements')db.exec('DROP TABLE candidate_retirements');
  journal.close();before=tables(config);await mkdir(path.join(config.runtimeRoot,'recovery-original'));await copyFile(path.join(config.runtimeRoot,'deployments.sqlite'),path.join(config.runtimeRoot,'recovery-original','deployments.sqlite'));
 }else if(input.group==='projects'){
  const agent={instanceId:'agent',serviceNodeId:'agent-node',componentId:'agent-manager',engine:'process',enabled:false,settings:{internalProjectRoot:path.join(root,'new-projects')}};config.instances.push(agent);original=structuredClone(config);original.instances[1].settings.internalProjectRoot=path.join(root,'old-projects');
  const retained=path.join(root,'retained');captured=[{key:'retained-projects/agent/'+hashJson(retained).slice(7),path:retained}];
  const index=path.join(servicePaths(config,agent).data,'project-backup-paths.json');await mkdir(path.dirname(index),{recursive:true});
  const paths=[path.join(root,'old-projects','one'),path.join(retained,'nested'),path.join(root,'unrelated')];
  await atomicJson(index,input.case==='malformed-index'?{}:paths);before={index:readFileSync(index,'utf8')};
 }else if(input.group==='maintenance'){
  const sourceTests=readFileSync(new URL('../storage-retention.test.ts',import.meta.url),'utf8'),ast=ts.createSourceFile('fixture.ts',sourceTests,ts.ScriptTarget.ESNext,true);
  const fixtures=ast.statements.filter(n=>ts.isFunctionDeclaration(n)&&['fixture','bootstrapFixture'].includes(n.name?.text)).map(n=>n.getText(ast)).join('\n');
  const js=ts.transpileModule(fixtures+'\nreturn bootstrapFixture;',{compilerOptions:{target:ts.ScriptTarget.ESNext}}).outputText;
  const {configurationBootstrapPlan,retainBootstrapPlan}=await import('../../packages/host-runtime/src/bootstrap.ts');
  const factory=new Function('mkdtemp','join','tmpdir','HostJournal','rm','mkdir','writeFile','utimes','configurationBootstrapPlan','retainBootstrapPlan','atomicJson','hashJson',js)(async()=>root,path.join,()=>root,HostJournal,async()=>{},mkdir,writeFile,async()=>{},configurationBootstrapPlan,retainBootstrapPlan,atomicJson,hashJson);
  const f=await factory({after(){}});Object.assign(config,f.config);f.journal.close();
  const request={schemaVersion:1,operationId:'restored-maintenance',next:f.plans[0],previous:{schemaVersion:1,hostId:config.hostId,installationId:f.plans[0].installationId,configPath:f.plans[0].configPath,configurationHash:f.plans[0].configHash,observedAt:at,owners:f.plans[0].processes.map(p=>({instanceId:p.instanceId,componentId:p.componentId,name:p.name,candidateId:p.candidateId??f.plans[0].candidateId,definitionHash:hashJson(p),enabled:true,running:false}))}};
  const record={schemaVersion:1,operationId:request.operationId,requestHash:hashJson(request),request,phase:input.case==='succeeded'?'succeeded':input.case==='needs-attention'?'needs_attention':'checking',createdAt:at,updatedAt:at,errorCode:null,definitions:[]};
  validateHost('BootstrapMaintenanceRecord',record);const name=hashJson(request.operationId).slice(7),dir=path.join(config.runtimeRoot,'bootstrap-maintenance',name);await mkdir(dir,{recursive:true});
  if(input.case==='invalid-identity')record.requestHash=hash('e');
  await atomicJson(path.join(dir,'record.json'),record);
  if(input.case==='unexpected-entry')await writeFile(path.join(config.runtimeRoot,'bootstrap-maintenance','unknown'),'unknown');
  before={name,record:readFileSync(path.join(dir,'record.json'),'utf8')};
 }else assert.fail('Unknown restore group');
 const setup={config,original,backupRoot,templatePath,manifest,manifestHash,captured,before,at,shared:[],exclusions:nativeBackupExclusions(config),localeOrder:config.instances.map(i=>i.instanceId).sort((a,b)=>a.localeCompare(b)),sourceHash:sha(source)};
 if(input.group==='journal')setup.beforeImageHash=sha(readFileSync(path.join(config.runtimeRoot,'recovery-original','deployments.sqlite')));
 await writeFile(setupPath,JSON.stringify(setup));process.stdout.write(JSON.stringify(setup));
}else{
 const setup=JSON.parse(readFileSync(setupPath,'utf8'));
 if(input.kind==='reset'){
  // Reset only this disposable fixture's working files to their saved exact
  // preimages, so the original runs at the same paths and identities. No source,
  // recovery authority, account root or production operation is replayed.
  const {unlink}=await import('node:fs/promises');
  if(input.group==='journal'){const p=path.join(setup.config.runtimeRoot,'deployments.sqlite');for(const suffix of['-wal','-shm'])if(existsSync(p+suffix))await unlink(p+suffix);await copyFile(path.join(setup.config.runtimeRoot,'recovery-original','deployments.sqlite'),p);}
  else if(input.group==='projects')await writeFile(path.join(servicePaths(setup.config,setup.config.instances[1]).data,'project-backup-paths.json'),setup.before.index);
  else if(input.group==='maintenance'){const p=path.join(setup.config.runtimeRoot,'recovery-original','bootstrap-maintenance',setup.before.name,'record.json');if(existsSync(p))await unlink(p);await writeFile(path.join(setup.config.runtimeRoot,'bootstrap-maintenance',setup.before.name,'record.json'),setup.before.record);}
  else assert.fail('Read-only groups need no fixture reset');
  process.stdout.write(JSON.stringify({reset:true}));
 }else if(input.kind==='run'){
  let outcome;
  if(input.group==='verify'){const {verifyBackup}=await import(restoreUrl.href);outcome=await wrap(()=>verifyBackup(setup.backupRoot,setup.manifestHash));}
  else if(input.group==='target'){const {targetConfiguration}=await import(restoreUrl.href);outcome=await wrap(()=>targetConfiguration(setup.templatePath,setup.backupRoot,setup.manifest,setup.manifestHash,setup.restoreId));}
  else if(input.group==='journal'){
   const start=source.indexOf('  const db = new DatabaseSync('),end=source.indexOf('\n  // Old launch-specific controls');assert(start>=0&&end>start);
   const body=source.slice(start,end);const js=ts.transpileModule(body,{compilerOptions:{target:ts.ScriptTarget.ESNext}}).outputText;
   const fn=new Function('config','createdAt','DatabaseSync','join','basename','terminalPhases','validateHost','canonical','installationRecord','requireThat',js);
   outcome=await wrap(()=>fn(setup.config,at,DatabaseSync,path.join,path.basename,terminalPhases,validateHost,canonical,installationRecord,requireThat));
  }else if(input.group==='projects'){const {relocateProjects}=await import('../../packages/host-runtime/src/project-restore.ts');outcome=await wrap(()=>relocateProjects(setup.original,setup.config,setup.captured));}
  else if(input.group==='maintenance'){const {holdRestoredBootstrapMaintenance}=await import('../../packages/host-runtime/src/bootstrap-maintenance.ts');outcome=await wrap(()=>holdRestoredBootstrapMaintenance(setup.config,at));}
  else assert.fail('Unknown restore operation');process.stdout.write(JSON.stringify(outcome));
 }else if(input.kind==='observe'){
  if(input.group==='journal')process.stdout.write(JSON.stringify({after:tables(setup.config),before:setup.before,beforeImagePreserved:sha(readFileSync(path.join(setup.config.runtimeRoot,'recovery-original','deployments.sqlite')))===setup.beforeImageHash}));
  else if(input.group==='projects'){const index=path.join(servicePaths(setup.config,setup.config.instances[1]).data,'project-backup-paths.json');process.stdout.write(JSON.stringify({index:JSON.parse(readFileSync(index,'utf8')),original:setup.original,captured:setup.captured}));}
  else if(input.group==='maintenance'){const file=path.join(setup.config.runtimeRoot,'bootstrap-maintenance',setup.before.name,'record.json'),original=path.join(setup.config.runtimeRoot,'recovery-original','bootstrap-maintenance',setup.before.name,'record.json');process.stdout.write(JSON.stringify({record:JSON.parse(readFileSync(file,'utf8')),original:existsSync(original)?JSON.parse(readFileSync(original,'utf8')):null,before:JSON.parse(setup.before.record),originalBytesPreserved:!existsSync(original)||readFileSync(original,'utf8')===setup.before.record}));}
  else if(input.group==='target')process.stdout.write(JSON.stringify({present:await Promise.all(setup.before.roots.map(exists)),before:setup.before.present}));
  else process.stdout.write(JSON.stringify({files:await inventoryFiles(setup.backupRoot)}));
 }else assert.fail('Unknown restore fixture kind');
}
