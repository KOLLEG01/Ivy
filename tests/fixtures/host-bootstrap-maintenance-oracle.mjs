import {existsSync,readFileSync} from 'node:fs';
import {readFile,writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}return next(specifier,context);}});
const input=JSON.parse(readFileSync(0,'utf8'));
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const {canonical,digest,hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {validateHost}=await import('../../packages/contracts/src/host-validation.ts');
const {atomicJson,jsonFile}=await import('../../packages/host-runtime/src/config.ts');
const {HostJournal}=await import('../../packages/host-runtime/src/journal.ts');
const {hostConfig}=await import('../../packages/host-runtime/src/host-config.ts');
const bootstrap=await import('../../packages/host-runtime/src/bootstrap.ts');
const maintenance=await import('../../packages/host-runtime/src/bootstrap-maintenance.ts');
const automatic=await import('../../packages/host-runtime/src/bootstrap-automatic.ts');
const {runAutomaticBootstrapUpdate}=await import('../../packages/host-runtime/src/bootstrap-auto-worker.ts');
const RealDate=Date;
globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[input.at]));}static now(){return input.at;}};
const wrap=async work=>{try{return{ok:true,value:(await work())??null};}catch(error){return{ok:false,error:IvyError.from(error).toWire()};}};
await atomicJson(input.configPath,input.config);
const config=await hostConfig(input.configPath);
if(canonical(config)!==canonical(input.config))throw Error('Oracle requires admitted exact HostConfig');
const journal=new HostJournal(config);
for(const pair of input.pairs){validateHost('Candidate',pair.candidate);validateHost('ReleaseManifest',pair.manifest);journal.saveCandidate(pair.candidate,pair.manifest);}
for(const value of input.installed)journal.setInstalled(value);
for(const plan of [input.oldPlan,input.nextPlan]){
  const candidate=journal.candidate(plan.candidateId);
  const actual=bootstrap.configurationBootstrapPlan(config,input.configPath,candidate,undefined,bootstrap.bootstrapComponents(config,journal,candidate));
  if(canonical(actual)!==canonical(plan))throw Error('Oracle exact candidate plan differs');
  validateHost('BootstrapPlan',plan);
}
await bootstrap.retainBootstrapPlan(input.oldPlan);
await bootstrap.publishBootstrapInstanceConfigurations(input.oldPlan,config,journal);
await atomicJson(join(config.runtimeRoot,'bootstrap',input.oldPlan.installationId+'.json'),input.oldPlan);
journal.syncBootstrap(input.oldPlan);
const os={owners:structuredClone(input.owners),writes:0,inspections:0,reloads:0,pauses:0,resumes:0,failAfterWrite:false,forbidAccess:false,
  async inspect(){if(this.forbidAccess)throw new IvyError('fixture_os_forbidden','Historical replay accessed the OS.');this.inspections++;return structuredClone(this.owners);},
  async render(owner,next){const candidateId=next.processes.find(p=>p.instanceId===owner.instanceId).candidateId;const content=canonical({...JSON.parse(owner.definition.content),candidateId});return{content,hash:digest(content)};},
  async check(definition){if(digest(definition.content)!==definition.hash)throw new IvyError('storage_invalid','Changed simulated definition.');},
  async apply(owner,desired){const current=this.owners.find(p=>p.instanceId===owner.instanceId);if(current.running||current.enabled||![owner.definitionHash,desired.hash].includes(current.definitionHash))throw new IvyError('target_conflict','Simulated definition changed.');if(current.definitionHash===desired.hash)return;
    current.definition=structuredClone(desired);current.definitionHash=desired.hash;current.candidateId=JSON.parse(desired.content).candidateId;this.writes++;
    if(this.changeConfig){this.changeConfig=false;await atomicJson(input.configPath,{...config,publicBaseUrl:'http://127.0.0.1:39081/changed'});}
    if(this.failAfterWrite){this.failAfterWrite=false;throw new IvyError('outcome_unknown','Simulated process loss after the OS write and before its recorded outcome.','unknown');}},
  async reload(){this.reloads++;},
  async pause(){this.pauses++;for(const owner of this.owners){owner.enabled=false;owner.running=false;}if(this.failPause){this.failPause=false;if(input.pauseThirdState){const owner=this.owners[0];owner.definition.content+='third';owner.definition.hash=digest(owner.definition.content);owner.definitionHash=owner.definition.hash;}throw new IvyError('bootstrap_os_unavailable','Simulated partial pause.');}},
  async resume(snapshot){this.resumes++;for(const owner of this.owners){const original=snapshot.owners.find(p=>p.instanceId===owner.instanceId);owner.enabled=original.enabled;owner.running=original.running;}},
};
const initial=await maintenance.inspectBootstrap(input.configPath,input.distribution,os);
if(canonical(initial)!==canonical(input.request.previous))throw Error('Original snapshot differs before effects');
const results=[],trace=[],readiness=[];
let savedOwner,savedEvidence;
const evidence=join(config.runtimeRoot,'bootstrap-maintenance',hashJson(input.request.operationId).slice(7),input.owners[0].instanceId+'.desired.json');
try{
  if(input.mode==='worker'){
    const requestPath=join(automatic.automaticBootstrapRoot(config,input.request.operationId),'request.json');
    await atomicJson(requestPath,input.request);
    const record={schemaVersion:1,operationId:input.request.operationId,requestHash:hashJson(input.request),phase:input.starting??'launched',errorCode:null,createdAt:new Date().toISOString(),updatedAt:new Date().toISOString()};
    await automatic.saveAutomaticBootstrapRecord(config,record);
    os.failPause=!!input.failPause;
    const ready=async(_config,plan,states,notBefore)=>{
      readiness.push({plan:plan.candidateId,states,notBefore});
      if(input.mutateRequest){const edited=structuredClone(input.request);for(const owner of edited.previous.owners){owner.enabled=false;owner.running=false;}await atomicJson(requestPath,edited);}
      if(input.failReadiness&&plan.candidateId===input.nextPlan.candidateId)throw new IvyError('readiness_failed','Simulated updated bootstrap readiness failure.');
      if(input.failRollbackReadiness&&plan.candidateId===input.oldPlan.candidateId)throw new IvyError('readiness_failed','Simulated prior bootstrap readiness failure.');
    };
    results.push(await wrap(()=>runAutomaticBootstrapUpdate(input.configPath,requestPath,input.distribution,os,ready)));
    trace.push({record:await automatic.automaticBootstrapRecord(config,input.request.operationId),owners:structuredClone(os.owners)});
    if(input.repeat){os.forbidAccess=true;results.push(await wrap(()=>runAutomaticBootstrapUpdate(input.configPath,requestPath,input.distribution,os,ready)));}
  }else for(const step of input.steps){
    if(step==='pause'){await os.pause();continue;}
    if(step==='fail'){os.failAfterWrite=true;continue;}
    if(step==='third'){const owner=os.owners.at(-1);savedOwner=structuredClone(owner);owner.definition.content+='third';owner.definition.hash=digest(owner.definition.content);owner.definitionHash=owner.definition.hash;continue;}
    if(step==='repair-owner'){os.owners[os.owners.length-1]=savedOwner;continue;}
    if(step==='tamper'){savedEvidence=await readFile(evidence,'utf8');await writeFile(evidence,savedEvidence.replace('principal, triggers and settings','tampered'));continue;}
    if(step==='repair-evidence'){await writeFile(evidence,savedEvidence);continue;}
    if(step==='change-config'){os.changeConfig=true;continue;}
    if(step==='restore-config'){await atomicJson(input.configPath,config);continue;}
    if(step==='forbid'){os.forbidAccess=true;continue;}
    let result;
    if(step==='update')result=await wrap(()=>maintenance.updateBootstrap(input.configPath,input.request,input.distribution,os));
    else if(step==='later'){const request={...input.request,operationId:'maintenance-later',previous:await maintenance.inspectBootstrap(input.configPath,input.distribution,os),next:input.oldPlan};result=await wrap(()=>maintenance.updateBootstrap(input.configPath,request,input.distribution,os));}
    else if(step==='conflict')result=await wrap(()=>maintenance.updateBootstrap(input.configPath,{...input.request,next:input.oldPlan},input.distribution,os));
    else if(step==='status')result=await wrap(()=>maintenance.bootstrapStatus(config,input.request.operationId));
    else throw Error('Unknown fixture step');
    results.push(result);trace.push({owners:structuredClone(os.owners),writes:os.writes,inspections:os.inspections,reloads:os.reloads});
  }
  const files=[];for(const process of input.nextPlan.processes){try{files.push(await jsonFile(process.configPath));}catch(error){if(error.code!=='ENOENT')throw error;files.push(null);}}
  const installed=config.instances.map(instance=>journal.installed(instance.instanceId));
  const selected=await jsonFile(join(config.runtimeRoot,'bootstrap',input.oldPlan.installationId+'.json'));
  process.stdout.write(JSON.stringify({initial,results,trace,readiness,files,installed,selected,writes:os.writes,pauses:os.pauses,resumes:os.resumes}));
}finally{journal.close();}
