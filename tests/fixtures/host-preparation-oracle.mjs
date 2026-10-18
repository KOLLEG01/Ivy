import {existsSync,readFileSync,writeFileSync,mkdirSync,readdirSync,lstatSync} from 'node:fs';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}return next(specifier,context);}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const input=JSON.parse(readFileSync(0,'utf8'));
const result=async work=>{try{return {ok:true,value:await work()};}catch(error){return {ok:false,error:IvyError.from(error).toWire()};}};
const optional=work=>{try{return work();}catch{return null;}};
const json=file=>JSON.parse(readFileSync(file,'utf8'));
let value;
if(input.kind==='progress'){
  const OriginalDate=Date;let at=input.now;
  globalThis.Date=class extends OriginalDate{constructor(...args){super(...(args.length?args:[at]));}static now(){return at;}};
  Object.defineProperty(process,'pid',{value:input.pid});
  const {PreparationProgress,preparationProgress}=await import('../../packages/host-runtime/src/preparation-progress.ts');
  const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
  const file=path.join(input.host.runtimeRoot,'preparation-progress',hashJson({componentId:input.component,snapshotId:input.snapshot}).slice(7)+'.json');
  if(input.action==='writer'){
    value=await result(async()=>{const progress=new PreparationProgress(input.host,input.component,input.snapshot);try{for(const action of input.operations){at=action.at;if(action.kind==='step')await progress.step(action.step,action.options);else if(action.kind==='output')progress.output('stdout',action.text);else throw Error('Unknown progress action');}}finally{await progress.close();}return optional(()=>json(file));});
  }else{
    mkdirSync(path.dirname(file),{recursive:true});writeFileSync(file,Object.hasOwn(input,'raw')?input.raw:JSON.stringify(input.value));
    value=await result(()=>preparationProgress(input.host,input.entry));
  }
}else if(input.kind==='preparation'){
  const {HostJournal}=await import('../../packages/host-runtime/src/journal.ts');
  const {compactPreparations,recoverPreparedBuild,preparationIdForBuild}=await import('../../packages/host-runtime/src/preparations.ts');
  const journal=new HostJournal(input.host);
  try{
    const operation=await result(()=>input.operation==='recover'?recoverPreparedBuild(journal,input.buildId):compactPreparations(input.host,input.onlyBuildId??undefined,input.measure??true));
    const root=path.join(input.host.stagingRoot,'build-'+preparationIdForBuild(input.buildId));
    value={operation,record:optional(()=>json(path.join(root,'preparation.json'))),before:optional(()=>json(path.join(root,'preparation-before-recovery.json'))),
      entries:optional(()=>readdirSync(root).sort()),candidate:optional(()=>journal.candidate(input.buildId)),
      snapshotBytes:optional(()=>readFileSync(input.snapshotFile,'utf8')),outsideBytes:optional(()=>readFileSync(input.outsideFile,'utf8'))};
  }finally{journal.close();}
}else if(input.kind==='lock'){
  const {ExecutorLock}=await import('../../packages/host-runtime/src/journal.ts');
  value=await result(async()=>{const lock=new ExecutorLock(input.path,input.shared);lock.close();return null;});
}else if(input.kind==='touch'){
  const OriginalDate=Date;globalThis.Date=class extends OriginalDate{constructor(...args){super(...(args.length?args:[input.now]));}};
  const {touchDependencyCache}=await import('../../packages/host-runtime/src/dependency-cache.ts');
  value=await result(async()=>{await touchDependencyCache(input.path);return optional(()=>lstatSync(input.path).mtimeMs);});
}else throw Error('Unknown preparation oracle mode');
process.stdout.write(JSON.stringify(value));
