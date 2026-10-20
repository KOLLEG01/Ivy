import assert from 'node:assert/strict';
import {readFileSync,existsSync} from 'node:fs';
import {mkdir,writeFile,readFile,rm,readdir,lstat} from 'node:fs/promises';
import {registerHooks,createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {DatabaseSync} from 'node:sqlite';
const input=JSON.parse(readFileSync(0,'utf8')),boundary=path.resolve(input.fixtureRoot),root=path.join(boundary,'reset-local');
assert(path.basename(boundary).startsWith('ivy-config-'));
const setupPath=path.join(root,'setup.json'),ts=createRequire(import.meta.url)(process.env.IVY_TEST_TYPESCRIPT_MODULE??'typescript');
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')){const url=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(url)))return next(url.href,context);}return next(specifier,context);}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {validateHost}=await import('../../packages/contracts/src/host-validation.ts');
const {atomicJson,jsonFile}=await import('../../packages/host-runtime/src/config.ts');
const {HostJournal,ExecutorLock,bootstrapLaunchId}=await import('../../packages/host-runtime/src/journal.ts');
const {runtimeResetMarker}=await import('../../packages/host-runtime/src/runtime-maintenance.ts');
const source=readFileSync(new URL('../../packages/host-runtime/src/runtime-reset.ts',import.meta.url),'utf8');
const ast=ts.createSourceFile('reset.ts',source,ts.ScriptTarget.ESNext,true);
const names=['savedRuntime','restoredBootstrapTarget','prepareLocal','completeLocal','confirmLocal','proveLocksReleased'];
const functions=ast.statements.filter(n=>ts.isFunctionDeclaration(n)&&names.includes(n.name?.text)).map(n=>n.getText(ast).replace(/^export\s+/,''));assert.equal(functions.length,names.length);
const variables=ast.statements.filter(n=>ts.isVariableStatement(n)&&n.declarationList.declarations.some(d=>['resetDirectory','recordPath'].includes(d.name.getText(ast)))).map(n=>n.getText(ast));assert.equal(variables.length,2);
const original=functions.join('\n'),admitted=original.replaceAll('  await requireAdministrator();\n','').replace('  await stopHost(config);\n','');
assert.equal((original.match(/await requireAdministrator\(\);/g)??[]).length,3);assert.equal((original.match(/await stopHost\(config\);/g)??[]).length,1);
assert(!admitted.includes('await requireAdministrator();')&&!admitted.includes('await stopHost(config);'));
const sha=b=>'sha256:'+createHash('sha256').update(b).digest('hex'),at='2026-10-20T12:00:00.000Z',hash=c=>'sha256:'+c.repeat(64);
const wrap=async fn=>{try{return{ok:true,value:(await fn())??null};}catch(error){return{ok:false,error:IvyError.from(error).toWire()};}};
const resetRoot=config=>path.dirname(runtimeResetMarker(config)),recordPath=config=>path.join(resetRoot(config),'current.json');
const clock=()=>{let tick=0;const NativeDate=Date;return class extends NativeDate{constructor(...args){super(...(args.length?args:[at]));}static now(){return (tick++)*20_000;}};};
function implementation(){
 const scope={join:path.join,dirname:path.dirname,mkdir,jsonFile,atomicJson,hashJson,requireThat,HostJournal,ExecutorLock,runtimeResetMarker,bootstrapLaunchId,validateHost,Date:clock(),setTimeout};
 const body=ts.transpileModule(variables.join('\n')+'\n'+admitted+'\nreturn {savedRuntime,restoredBootstrapTarget,prepareLocal,completeLocal,confirmLocal,proveLocksReleased};',{compilerOptions:{target:ts.ScriptTarget.ESNext}}).outputText;
 return new Function(...Object.keys(scope),body)(...Object.values(scope));
}
async function safeReset(setup){
 const target=path.resolve(resetRoot(setup.config));assert(target.startsWith(boundary+path.sep));assert.notEqual(target,boundary);
 await rm(target,{recursive:true,force:true});await mkdir(target,{recursive:true});
 if(setup.recordDirectory)await mkdir(recordPath(setup.config));
 else if(setup.recordBytes!==null)await writeFile(recordPath(setup.config),Buffer.from(setup.recordBytes,'base64'));
 if(setup.markerDirectory)await mkdir(runtimeResetMarker(setup.config));
 else if(setup.markerBytes!==null)await writeFile(runtimeResetMarker(setup.config),Buffer.from(setup.markerBytes,'base64'));
}
async function observe(setup){
 const file=async p=>{try{const m=await lstat(p);return m.isDirectory()?{kind:'directory'}:{kind:'file',base64:(await readFile(p)).toString('base64')};}catch(error){if(error.code==='ENOENT')return null;throw error;}};
 const db=new DatabaseSync(path.join(setup.config.runtimeRoot,'deployments.sqlite'),{readOnly:true});let authority;
 try{authority={candidates:db.prepare('SELECT candidate_json,manifest_json FROM candidates ORDER BY candidate_id').all(),installed:db.prepare('SELECT value FROM installed ORDER BY instance_id').all()};}finally{db.close();}
 const recordRoot=resetRoot(setup.config),members=await readdir(recordRoot).catch(error=>{if(error.code==='ENOENT')return[];throw error;});
 const locks=[];
 for(const name of ['executor',...setup.config.instances.map(i=>'owners/'+i.instanceId)]){
  const p=path.join(setup.config.runtimeRoot,name,'executor-lock.sqlite');if(!existsSync(p))continue;
  const db=new DatabaseSync(p,{readOnly:true});try{const rows=db.prepare('SELECT pid FROM owner').all();locks.push({name,owners:rows.length,positivePid:rows.every(r=>Number.isInteger(r.pid)&&r.pid>0)});}finally{db.close();}
 }
 return{record:await file(recordPath(setup.config)),marker:await file(runtimeResetMarker(setup.config)),members:members.filter(p=>!p.endsWith('.tmp')).sort(),temporaryFiles:members.filter(p=>p.endsWith('.tmp')).length,authority,locks};
}
if(input.kind==='target'){
 const impl=implementation();process.stdout.write(JSON.stringify(await wrap(()=>impl.restoredBootstrapTarget(input.plan,input.instanceId,input.candidateId,input.enabled,input.at))));
}else if(input.kind==='setup'){
 await mkdir(root,{recursive:true});const config=input.config;
 assert(path.resolve(config.runtimeRoot).startsWith(boundary+path.sep));
 if(input.case==='fallback-root')delete config.ivyRoot;
 else config.ivyRoot=path.join(boundary,'installation');
 if(input.group==='locks')config.instances=['first','middle','last'].map(instanceId=>({...config.instances[0],instanceId,serviceNodeId:instanceId}));
 const journal=new HostJournal(config);
 try{
  if(input.group==='saved'&&input.case==='none'){}
  else{journal.saveCandidate(input.candidate,input.manifest);journal.setInstalled({instanceId:config.instances[0].instanceId,candidateId:input.candidate.candidateId,buildId:input.candidate.buildId,enabled:input.case!=='disabled',installedAt:at});}
 }finally{journal.close();}
 if(input.group==='saved'&&input.case==='missing-candidate'){const db=new DatabaseSync(path.join(config.runtimeRoot,'deployments.sqlite'));try{db.exec('DELETE FROM candidates');}finally{db.close();}}
 const plan={schemaVersion:1,deploymentIdentity:'fixture-deployment',hostId:config.hostId,configPath:path.join(boundary,'config.json'),instances:config.instances.map(i=>({instanceId:i.instanceId,componentId:i.componentId,enabled:i.enabled})),scopeRootIds:['scope-one'],delete:[],preserve:[],completionMarkers:[]};
 const args={resetId:'reset-one',runtimeEpoch:'epoch-one',publisherDigest:hash('d')};
 const previous={schemaVersion:1,deploymentIdentity:plan.deploymentIdentity,resetId:args.resetId,runtimeEpoch:args.runtimeEpoch,hostId:config.hostId,phase:'prepared',updatedAt:'2026-01-01T00:00:00.000Z',planHash:hashJson(plan),plan,saved:[{instanceId:'retained-authority',enabled:false}],instructions:[{retained:'instruction authority'}],publisherCredentialDigest:args.publisherDigest,instructionsCaptured:true,deploymentComplete:false,retainedExtra:{originalOperationId:'original-reset-operation'}};
 let record=null,recordBytes=null,markerBytes=null,recordDirectory=false,markerDirectory=false;
 if(input.group==='prepare'){
  if(input.case.startsWith('retained-')){record=structuredClone(previous);record.phase=input.case.slice(9);}
  if(input.case==='retained-missing-instructions'){record=structuredClone(previous);record.instructions=null;}
  if(input.case.startsWith('conflicting-')){record=structuredClone(previous);const key={'conflicting-reset':'resetId','conflicting-epoch':'runtimeEpoch','conflicting-plan':'planHash','conflicting-publisher':'publisherCredentialDigest'}[input.case];assert(key);record[key]='different';}
  if(input.case==='completed-new-identity'){record=structuredClone(previous);record.deploymentComplete=true;record.resetId='old-reset';}
  if(input.case==='completed-same-identity'){record=structuredClone(previous);record.deploymentComplete=true;record.phase='released';}
  if(input.case==='previous-null')recordBytes=Buffer.from('null').toString('base64');
  if(input.case==='previous-falsy')recordBytes=Buffer.from('0').toString('base64');
  if(input.case==='malformed-record')recordBytes=Buffer.from('{invalid').toString('base64');
  if(input.case==='invalid-digest')args.publisherDigest='sha256:'+ 'D'.repeat(64);
  if(input.case==='record-failure')recordDirectory=true;
  if(input.case==='marker-failure')markerDirectory=true;
 }else if(input.group==='complete'||input.group==='confirm'){
  record=structuredClone(previous);record.phase=input.group==='complete'?'cleared':'released';
  if(input.case==='wrong-reset')args.resetId='different';
  if(input.case==='wrong-epoch')args.runtimeEpoch='different';
  if(['prepared','cleared','complete','released'].includes(input.case))record.phase=input.case;
  if(input.case==='already-confirmed')record.deploymentComplete=true;
  if(input.case==='truthy-completion')record.deploymentComplete=[];
  if(input.case==='null-record'){record=null;recordBytes=Buffer.from('null').toString('base64');}
  if(input.case==='missing-record')record=null;
  if(input.case==='malformed-record'){record=null;recordBytes=Buffer.from('{invalid').toString('base64');}
  markerBytes=Buffer.from('retained maintenance hold').toString('base64');
 }
 if(record!==null)recordBytes=Buffer.from(JSON.stringify(record)).toString('base64');
 const setup={config,plan,args,at,recordBytes,markerBytes,recordDirectory,markerDirectory,sourceHash:sha(source),declarationHash:sha(functions.join('\n')+variables.join('\n')),admittedCutHash:sha(admitted),excludedStatements:['requireAdministrator (3 original phase entries)','stopHost (prepare tail only)']};
 await safeReset(setup);await writeFile(setupPath,JSON.stringify(setup));process.stdout.write(JSON.stringify(setup));
}else{
 const setup=JSON.parse(await readFile(setupPath,'utf8'));
 if(input.kind==='reset'){await safeReset(setup);process.stdout.write(JSON.stringify({reset:true}));}
 else if(input.kind==='observe')process.stdout.write(JSON.stringify(await observe(setup)));
 else if(input.kind==='run'){
  const impl=implementation();const held=[];
  if(input.group==='locks'&&input.case.startsWith('held-')){const name=input.case==='held-executor'?'executor':'owners/'+(input.case==='held-first'?'first':'last');held.push(new ExecutorLock(path.join(setup.config.runtimeRoot,name)));}
  let result;
  try{result=await wrap(async()=>{
   if(input.group==='saved')return impl.savedRuntime(setup.config);
   if(input.group==='prepare')return impl.prepareLocal(setup.config,setup.plan,setup.args.resetId,setup.args.runtimeEpoch,setup.args.publisherDigest);
   if(input.group==='complete')return impl.completeLocal(setup.config,setup.args.resetId,setup.args.runtimeEpoch);
   if(input.group==='confirm')return impl.confirmLocal(setup.config,setup.args.resetId,setup.args.runtimeEpoch);
   if(input.group==='locks')return impl.proveLocksReleased(setup.config);
   assert.fail('Unknown reset stage');
  });}finally{for(const lock of held)lock.close();}
  process.stdout.write(JSON.stringify(result));
 }else assert.fail('Unknown local reset operation');
}
