// Execute the original helpers and validators against isolated caller-owned files.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
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
const rows=[];
for(const input of JSON.parse(readFileSync(0,'utf8'))){
  try {
    const value=input.action==='backup'?await snapshotStoppedHive(input.source,input.destination):inspectStoppedStorage(input.path,input.requirements);
    rows.push({value});
  }catch(error){rows.push({error:IvyError.from(error).toWire()});}
}
process.stdout.write(JSON.stringify(rows));
