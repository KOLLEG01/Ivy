import {readFileSync,writeFileSync} from 'node:fs';
import {createRequire,registerHooks} from 'node:module';
import {pathToFileURL} from 'node:url';
import {AsyncLocalStorage} from 'node:async_hooks';
import {randomUUID} from 'node:crypto';
const require=createRequire(process.env.IVY_TEST_ORIGINAL_DEPENDENCIES+'/package.json');
registerHooks({resolve(specifier,context,next){
 if(specifier.startsWith('.')&&specifier.endsWith('.js')){
  const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
  try{readFileSync(source);return next(source.href,context);}catch{}
 }
 try{return next(specifier,context);}catch(error){if(/^ajv(?:\/|$)/.test(specifier))return next(pathToFileURL(require.resolve(specifier)).href,context);throw error;}
}});
const ts=require(process.env.IVY_TEST_TYPESCRIPT_MODULE),source=readFileSync(new URL('../../packages/host-runtime/src/executor.ts',import.meta.url),'utf8');
const ast=ts.createSourceFile('executor.ts',source,ts.ScriptTarget.Latest,true,ts.ScriptKind.TS);
// Execute the actual declaration bodies with real HostJournal/target/config
// dependencies. Constructor/CLI/updater imports are outside this activation cut.
const declarations=ast.statements.filter(node=>ts.isFunctionDeclaration(node)||ts.isClassDeclaration(node)&&node.name?.text==='HostExecutor'||ts.isVariableStatement(node)&&node.declarationList.declarations.some(d=>['now','fresh'].includes(d.name.getText(ast))));
const {HostJournal,terminalPhases}=await import('../../packages/host-runtime/src/journal.ts');
const {materializeTarget,stoppedTarget}=await import('../../packages/host-runtime/src/target.ts');
const {readAcceptedInstanceConfiguration}=await import('../../packages/host-runtime/src/accepted-configuration.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {canonical}=await import('../../packages/contracts/src/canonical.ts');
globalThis.__ivyExecutorBoundary={HostJournal,terminalPhases,materializeTarget,stoppedTarget,readAcceptedInstanceConfiguration,IvyError,requireThat,canonical,AsyncLocalStorage,randomUUID};
const prelude='const {HostJournal,terminalPhases,materializeTarget,stoppedTarget,readAcceptedInstanceConfiguration,IvyError,requireThat,canonical,AsyncLocalStorage,randomUUID}=globalThis.__ivyExecutorBoundary;\n';
const code=ts.transpileModule(prelude+declarations.map(node=>node.getText(ast)).join('\n')+'\nexport {fresh};',{compilerOptions:{target:ts.ScriptTarget.ES2022,module:ts.ModuleKind.ES2022}}).outputText;
const original=await import('data:text/javascript;base64,'+Buffer.from(code).toString('base64'));
const input=JSON.parse(readFileSync(0,'utf8'));
if(input.kind==='policy'){
 const value=input.cases.map(c=>{
  switch(c.operation){
   case 'fresh':Date.now=()=>c.at;return original.fresh(c.args[0]);
   case 'readinessFailureStart':return original.readinessFailureStart(...c.args);
   case 'readinessExpired':return original.readinessExpired(...c.args);
   case 'replacementNeedsSettledObservation':return original.replacementNeedsSettledObservation(...c.args);
   case 'stoppedTargetIsConclusive':return original.stoppedTargetIsConclusive(...c.args);
   case 'runningHiveCanBeInspected':return original.runningHiveCanBeInspected(...c.args);
   default:throw Error('Unknown original policy case');
  }
 });process.stdout.write(JSON.stringify(value));
}else{
 const journal=new HostJournal(input.config),trace=[];let recording=false,currentId;
 const executor=Object.create(original.HostExecutor.prototype);
 Object.assign(executor,{baseJournal:journal,executionJournal:{getStore:()=>journal},controller:new AbortController(),ownInstanceId:'independent-executor'});
 const phase=()=>journal.get(currentId).record.phase;
 executor.compatibility=async(candidate,instance,settings)=>{
  if(recording)trace.push({operation:'compatibility',phase:phase(),candidate,tool:journal.config.executables.tool,settings:settings??null});
  if(recording&&((input.scenario==='precheck'&&phase()==='checking')||(input.scenario==='postdrain'&&phase()==='draining')||(input.scenario==='rollback-incompatible'&&settings)))throw new IvyError('incompatible_contract','Fixture compatibility refused.');
 };
 executor.waitFor=async(instance,target,predicate,timeout,allowStart=false)=>{
  if(recording)trace.push({operation:'reconcile',phase:phase(),candidate:target?.candidateId??null,desired:target?.desired??null,allowStart});
  let state=!target?'idle':target.desired==='stopped'?'stopped':'ready';
  if(recording&&['rollback','rollback-incompatible','no-previous-failure'].includes(input.scenario)&&target?.candidateId===input.badId&&target.desired==='running')state='failed';
  const at=new Date().toISOString();
  const health=target&&state==='ready'?{schemaVersion:1,instanceId:instance,componentId:journal.instance(instance).componentId,buildId:target.candidateId,pid:1,bootId:'fixture-boot',startedAt:at,observedAt:at,ready:true,generation:null,launchId:target.revision,details:''}:null;
  journal.recordObservation(instance,{schemaVersion:1,instanceId:instance,ownerPid:1,ownerBootId:'fixture-owner',observedAt:at,targetRevision:target?.revision??null,candidateId:target?.candidateId??null,buildId:target?.candidateId??null,state,health,restartCount:0,nextRestartAt:null,code:state==='failed'?'fixture_failed':null,message:state==='failed'?'Fixture launch failed.':''});
  return original.HostExecutor.prototype.waitFor.call(executor,instance,target,predicate,timeout,allowStart);
 };
 try{
  for(const row of input.candidates)journal.saveCandidate(row.candidate,row.manifest);
  if(['rollback','rollback-incompatible'].includes(input.scenario)){
   const first=journal.accept({action:'deploy',instanceId:'fixture',candidateId:input.goodId,operationId:'initial'});currentId=first.record.deploymentId;await executor.execute(first);
   requireThat(journal.get(currentId).record.phase==='succeeded','internal','Initial original activation failed.');
  }
  const desired=structuredClone(input.config);desired.executables.tool='updated-tool';desired.instances[0].settings={marker:'updated'};journal.useConfiguration(desired);
  const request={action:'deploy',instanceId:'fixture',candidateId:input.scenario==='success'||input.scenario==='disabled'||input.scenario==='precheck'||input.scenario==='postdrain'?input.goodId:input.badId,operationId:'selected'};
  if(input.scenario==='disabled')desired.instances[0].enabled=false;
  journal.useConfiguration(desired);
  const entry=journal.accept(request);currentId=entry.record.deploymentId;
  if(['rollback','rollback-incompatible'].includes(input.scenario)){
   const file=entry.activation.previousTarget.configPath,value=JSON.parse(readFileSync(file,'utf8'));value.settings={marker:'damaged'};writeFileSync(file,JSON.stringify(value));
  }
  recording=true;await executor.execute(entry);
  const final=journal.get(currentId),target=journal.target('fixture'),installed=journal.installed('fixture');
  const value={phase:final.record.phase,errorCode:final.record.errorCode,readiness:final.record.readiness,observedBuild:final.record.observedBuild,target:target?{candidateId:target.candidateId,desired:target.desired,settings:JSON.parse(readFileSync(target.configPath,'utf8')).settings}:null,installed:installed?{candidateId:installed.candidateId,buildId:installed.buildId,enabled:installed.enabled}:null,rollbackRevisionMatches:final.activation.rollbackTargetRevision===null?null:final.activation.rollbackTargetRevision===target?.revision,trace};
  process.stdout.write(JSON.stringify(value));
 }finally{journal.close();}
}
