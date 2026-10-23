import {existsSync,readFileSync} from 'node:fs';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}return next(specifier,context);}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const input=JSON.parse(readFileSync(0,'utf8'));
if(input.kind==='credentials'){
  const source=readFileSync(new URL('../../packages/host-runtime/src/executor.ts',import.meta.url),'utf8');
  const start=source.indexOf('const updateCredential ='),end=source.indexOf('this.packageUpdater',start);
  if(start<0||end<start)throw Error('Original credential source span changed');
  const select=new Function('config','ownInstanceId','packageCredential',source.slice(start,end)+'return updateCredential;');
  process.stdout.write(JSON.stringify(input.cases.map(c=>select(input.config,c.own??undefined,Object.hasOwn(c,'explicit')?c.explicit:undefined)??null)));
}else{
  const {ConfigurationUpdater}=await import('../../packages/host-runtime/src/configuration-updater.ts');
  const {HostJournal}=await import('../../packages/host-runtime/src/journal.ts');
  const {checkedHostConfig}=await import('../../packages/host-runtime/src/host-config.ts');
  const {atomicJson,jsonFile}=await import('../../packages/host-runtime/src/config.ts');
  const initial=await checkedHostConfig(input.config,input.configPath);
  await atomicJson(input.configPath,initial);
  let journal=new HostJournal(initial),calls=[],remote=input.remote;
  journal.saveCandidate(input.candidate,input.manifest);
  for(const installed of input.installed)journal.setInstalled(installed);
  if(input.busy)journal.accept(input.busy);
  let updater;
  const bind=()=>{updater=new ConfigurationUpdater(input.configPath,journal,'fixture-credential');updater.client.request=async(method,params,options)=>{calls.push({method,params,timeoutMs:options.timeoutMs});if(input.remoteError)throw new IvyError(input.remoteError,'Fixture remote failure.');return remote;};};
  bind();const results=[];
  for(const step of input.steps){
    if(step.remote)remote=step.remote;
    if(step.failInstance)journal.db.exec(`CREATE TRIGGER fixture_fail_accept BEFORE INSERT ON operations WHEN json_extract(NEW.entry_json,'$.request.instanceId')='${step.failInstance}' BEGIN SELECT RAISE(ABORT,'fixture acceptance interruption'); END`);
    if(step.removeFailure)journal.db.exec('DROP TRIGGER fixture_fail_accept');
    if(step.reopen){journal.close();journal=new HostJournal(await jsonFile(input.configPath));bind();}
    let result;
    try{result={ok:true,value:await updater.sync()};}catch(error){result={ok:false,error:IvyError.from(error).toWire()};}
    results.push({result,config:journal.config,file:await jsonFile(input.configPath),pending:journal.configurationAdoption(),requests:journal.list().map(e=>e.request).sort((a,b)=>a.operationId<b.operationId?-1:a.operationId>b.operationId?1:0),calls:[...calls]});
  }
  journal.close();process.stdout.write(JSON.stringify(results));
}
