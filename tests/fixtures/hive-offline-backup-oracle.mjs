// Run the original CLI body with its actual SQLite helper and explicit streams.
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
 if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const path=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(path)))return next(path.href,context);}
 return next(specifier,context);
},load(url,context,next){
 if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
 return next(url,context);
}});
const {snapshotStoppedHive}=await import('../../services/hive/src/offline-backup.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const source=readFileSync(new URL('../../services/hive/src/offline-backup.ts',import.meta.url),'utf8');
const at=source.indexOf('if (process.argv[1] && fileURLToPath'),start=source.indexOf('{',at)+1,end=source.lastIndexOf('}');
if(at<0||start<=at||end<start)throw Error('Original offline CLI boundaries changed');
const cli=new Function('process','requireThat','IvyError','snapshotStoppedHive','return(async()=>{'+stripTypeScriptTypes(source.slice(start,end),{mode:'strip'})+'})();');
const rows=[];
for(const args of JSON.parse(readFileSync(0,'utf8'))){
 const stdout=[],stderr=[],process={argv:['node','owner',...args],stdout:{write:value=>stdout.push(value)},stderr:{write:value=>stderr.push(value)},exitCode:0};
 await cli(process,requireThat,IvyError,snapshotStoppedHive);rows.push({status:process.exitCode,stdout:stdout.join(''),stderr:stderr.join('')});
}
process.stdout.write(JSON.stringify(rows));
