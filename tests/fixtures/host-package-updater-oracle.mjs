import {existsSync, readFileSync} from 'node:fs';
import {readFile, rm} from 'node:fs/promises';
import {join} from 'node:path';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
}});
const input=JSON.parse(readFileSync(0,'utf8'));
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const {validateHost}=await import('../../packages/contracts/src/host-validation.ts');
const {atomicJson,jsonFile}=await import('../../packages/host-runtime/src/config.ts');
const {bootstrapComponents}=await import('../../packages/host-runtime/src/bootstrap.ts');
const automatic=await import('../../packages/host-runtime/src/bootstrap-automatic.ts');
const wrap=async fn=>{try{return{ok:true,value:await fn()};}catch(error){return{ok:false,error:IvyError.from(error).toWire()};}};
const at=input.at??1760000000000;
const RealDate=Date;
globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[at]));}static now(){return at;}};
if(input.kind==='collation'){
  process.stdout.write(JSON.stringify([...new Set(input.values)].sort((a,b)=>a.localeCompare(b))));
}else if(input.kind==='record'){
  process.stdout.write(JSON.stringify(await wrap(async()=>{
    if(input.value!==undefined)await atomicJson(join(automatic.automaticBootstrapRoot(input.config,input.operationId),'record.json'),input.value);
    return {root:automatic.automaticBootstrapRoot(input.config,input.operationId),record:await automatic.automaticBootstrapRecord(input.config,input.operationId)};
  })));
}else{
  let config=input.config;
  const installed=new Map(input.installed.map(value=>[value.instanceId,value]));
  const pairs=new Map(input.pairs.map(value=>[value.candidate.candidateId,value]));
  for(const pair of pairs.values()){
    validateHost('Candidate',pair.candidate);validateHost('ReleaseManifest',pair.manifest);
  }
  validateHost('HostConfig',config);
  const requests=[],calls=[],results=[],events=[];let failInstance;
  const journal={
    get config(){return config;},
    installed:id=>installed.get(id)??null,
    candidate:id=>{const value=pairs.get(id);if(!value)throw new IvyError('not_found','Host-local candidate was not found.');return value.candidate;},
    manifest:id=>{const value=pairs.get(id);if(!value)throw new IvyError('not_found','Host-local candidate was not found.');if(input.invalidManifest===id)throw new IvyError('invalid_arguments','Fixture obsolete manifest.');return value.manifest;},
    unfinished:()=>input.busy?[{}]:[],
    saveCandidate:(candidate,manifest)=>pairs.set(candidate.candidateId,{candidate,manifest}),
    accept:request=>{if(request.instanceId===failInstance)throw Error('Fixture interrupted acceptance');if(!requests.some(value=>value.operationId===request.operationId))requests.push(request);},
    db:{prepare:()=>({all:component=>[...pairs.values()].filter(value=>value.candidate.componentId===component)
      .sort((a,b)=>a.candidate.candidateId<b.candidate.candidateId?-1:1).slice(0,1000).map(value=>({candidate_json:JSON.stringify(value.candidate)}))})},
  };
  if(input.kind==='components'){
    process.stdout.write(JSON.stringify(await wrap(()=>bootstrapComponents(config,journal,input.hostCandidate,input.preferred))));
  }else if(input.kind==='import'){
    const {PackageUpdater}=await import('../../packages/host-runtime/src/package-updater.ts');
    const updater=new PackageUpdater(config,journal,'fixture-credential');
    const result=await wrap(()=>updater.importPackage(input.entry));
    process.stdout.write(JSON.stringify({result,saved:[...pairs.values()]}));
  }else if(input.kind==='packages'){
    const {PackageUpdater}=await import('../../packages/host-runtime/src/package-updater.ts');
    const updater=new PackageUpdater(config,journal,'fixture-credential');
    let response;
    globalThis.fetch=async(_url,options)=>{
      calls.push({revision:Number(new URL(_url).searchParams.get('after')),base:config.publicBaseUrl,credential:options.headers.authorization.slice(7)});
      if(response.error)throw new IvyError(response.error,'Fixture catalog failure.');
      return new Response(response.status===204?null:JSON.stringify(response.value),{status:response.status??200});
    };
    if(!input.actualImport)updater.importPackage=async entry=>{
      if(input.importError)throw new IvyError(input.importError,'Fixture package import failed.');
      return journal.candidate(entry.buildId);
    };
    for(const step of input.steps){
      failInstance=step.failInstance;
      if(step.config)config=step.config;
      for(const value of step.installed??[])installed.set(value.instanceId,value);
      response=step;
      results.push(await wrap(()=>updater.sync()));
    }
    requests.sort((a,b)=>a.operationId<b.operationId?-1:1);
    process.stdout.write(JSON.stringify({results,requests,calls}));
  }else{
    let failLaunch=false;
    const os={async launchAutomatic(plan,requestPath,operationId){
      events.push({kind:'launch',plan,requestPath,operationId});
      if(failLaunch)throw new IvyError('outcome_unknown','Fixture automatic launch failed.','unknown');
    }};
    if(input.noLaunch)delete os.launchAutomatic;
    const inspect=async()=>{events.push({kind:'inspect'});return input.snapshot;};
    const updater=new automatic.AutomaticBootstrapUpdater(input.configPath,input.distribution,journal,os,inspect);
    let lastRequestPath;
    for(const step of input.steps){
      failLaunch=!!step.failLaunch;
      if(step.phase&&lastRequestPath){const record=await jsonFile(join(lastRequestPath,'..','record.json'));record.phase=step.phase;record.errorCode=step.errorCode??null;await automatic.saveAutomaticBootstrapRecord(config,record);}
      if(step.deleteRequest&&lastRequestPath)await rm(lastRequestPath);
      if(step.changeRequest&&lastRequestPath){const request=await jsonFile(lastRequestPath);request.previous.observedAt='2000-01-01T00:00:00.000Z';await atomicJson(lastRequestPath,request);}
      if(step.changeNext&&lastRequestPath){const request=await jsonFile(lastRequestPath);request.next.candidateId='sha256:'+'f'.repeat(64);await atomicJson(lastRequestPath,request);}
      results.push(await wrap(()=>updater.reconcile(input.selected)));
      lastRequestPath=events.findLast(value=>value.kind==='launch')?.requestPath??input.expectedRequestPath;
      const read=async file=>{try{return await jsonFile(file);}catch(error){if(error.code==='ENOENT')return null;throw error;}};
      if(lastRequestPath)calls.push({request:await read(lastRequestPath),record:await read(join(lastRequestPath,'..','record.json'))});
    }
    process.stdout.write(JSON.stringify({results,events,files:calls}));
  }
}
