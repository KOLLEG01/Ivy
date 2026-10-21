// Execute the original helpers and validators against isolated caller-owned files.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {join} from 'node:path';
registerHooks({
  resolve(specifier,context,next){
    if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){
      const path=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
      if(existsSync(fileURLToPath(path)))return next(path.href,context);
    }
    return next(specifier,context);
  },
  load(url,context,next){
    if(url.endsWith('.ts'))return {format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
    return next(url,context);
  }
});
const {inspectStoppedStorage}=await import('../../services/hive/src/inspect-storage.ts');
const {snapshotStoppedHive}=await import('../../services/hive/src/offline-backup.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const {requireThat}=await import('../../packages/contracts/src/errors.ts');
const {validateHost}=await import('../../packages/contracts/src/host-validation.ts');
const {setup}=await import('./hive-ui-files-fixture.mjs');
const source=readFileSync(new URL('../../services/hive/src/inspect-storage.ts',import.meta.url),'utf8');
const main=source.indexOf('if (process.argv[1] && fileURLToPath'),start=source.indexOf('{',main)+1,end=source.lastIndexOf('}');
if(main<0||start<main||end<start)throw Error('Original inspector CLI boundaries changed');
const originalCli=new Function('instanceConfig','configurationPath','requireThat','inspectStoppedStorage','join','validateHost','IvyError','process','return (async()=>{'+stripTypeScriptTypes(source.slice(start,end),{mode:'strip'})+'})();');
const rows=[];
for(const input of JSON.parse(readFileSync(0,'utf8'))){
  try {
    if(input.action==='fixture'){setup(input.root,input.actions);rows.push({value:null});continue;}
    if(input.action==='inspector'){
      const out=[],err=[],process={argv:['node','owner',...(input.args??[])],stdout:{write:value=>out.push(value)},stderr:{write:value=>err.push(value)},exitCode:0};
      // The caller supplies admitted configuration. Generic file/runtime/artifact
      // admission remains Host-owned; the original Hive owner body is unchanged.
      await originalCli(async()=>{validateHost('InstanceConfig',input.config);return input.config;},()=>null,requireThat,inspectStoppedStorage,join,validateHost,IvyError,process);
      rows.push({status:process.exitCode,stdout:out.join(''),stderr:err.join('')});continue;
    }
    const value=input.action==='backup'?await snapshotStoppedHive(input.source,input.destination):inspectStoppedStorage(input.path,input.requirements);
    rows.push({value});
  }catch(error){rows.push({error:IvyError.from(error).toWire()});}
}
process.stdout.write(JSON.stringify(rows));
