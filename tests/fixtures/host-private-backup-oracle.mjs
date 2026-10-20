import {readFileSync, existsSync} from 'node:fs';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
import {DatabaseSync, backup} from 'node:sqlite';
const input=JSON.parse(readFileSync(0,'utf8'));
if(input.style){Object.defineProperty(process,'platform',{value:input.style==='win32'?'win32':'linux'});process.env.HOME=input.home;process.env.USERPROFILE=input.home;process.cwd=()=>input.cwd;}
registerHooks({resolve(specifier,context,next){
  if(input.style&&specifier==='node:path'&&/\/(?:layout|config|project-storage)\.ts$/.test(context.parentURL??''))return next('node:path/'+input.style,context);
  if(specifier.startsWith('.')&&specifier.endsWith('.js')){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const wrap=async work=>{try{return {ok:true,value:(await work())??null};}catch(error){return {ok:false,error:IvyError.from(error).toWire()};}};
if(input.kind==='layout'){
  const layout=await import('../../packages/host-runtime/src/layout.ts'),project=await import('../../packages/host-runtime/src/project-storage.ts');
  const cases=[];for(const c of input.cases)cases.push(await wrap(()=>c.operation==='restoredStorageAreas'?project.restoredStorageAreas(c.config,c.captured):layout[c.operation](c.config,...(c.args??[]))));
  process.stdout.write(JSON.stringify({localeOrder:[...input.names].sort((a,b)=>a.localeCompare(b)),cases}));
}else{
  const root=path.resolve(input.fixtureRoot);
  if(!path.basename(root).startsWith('ivy-config-'))throw Error('Expected disposable fixture root');
  for(const p of [input.source,input.destination,input.configPath].filter(Boolean)){const local=path.relative(root,path.resolve(p));if(local==='..'||local.startsWith('..'+path.sep)||path.isAbsolute(local))throw Error('Backup fixture leaves disposable root');}
  if(input.kind==='nativePolicy'){
    // Execute the original public policy with only disposable account inputs.
    delete process.env.CODEX_HOME;
    if(Object.hasOwn(input.environment??{},'CODEX_HOME'))process.env.CODEX_HOME=input.environment.CODEX_HOME;
    const {nativeBackupExclusions,sharedNativeHomes}=await import('../../packages/host-runtime/src/native-backup-paths.ts');
    const rows=[],words=new Set(input.names??[]);
    for(const entry of input.cases){
      const exclusions=await wrap(()=>nativeBackupExclusions(entry.config));
      const sharedHomes=await wrap(()=>sharedNativeHomes(entry.config));
      const ordered=await wrap(()=>({ephemeralNativePaths:nativeBackupExclusions(entry.config),sharedNativeHomes:sharedNativeHomes(entry.config)}));
      rows.push({name:entry.name,exclusions,sharedHomes,ordered});
      for(const instance of entry.config.instances??[])if(typeof instance?.instanceId==='string')words.add(instance.instanceId);
      for(const value of [exclusions,sharedHomes])for(const row of value.value??[])if(typeof row.path==='string')words.add(row.path);
    }
    process.stdout.write(JSON.stringify({rows,localeOrder:[...words].sort((a,b)=>a.localeCompare(b))}));
  }else if(input.kind==='snapshot'){
    // Exact private function body, with only its built-in imports supplied. No
    // policy rewrite or copied snapshot implementation is used by the oracle.
    const source=readFileSync(new URL('../../packages/host-runtime/src/private-backup.ts',import.meta.url),'utf8');
    const body=source.slice(source.indexOf('async function snapshotJournal('),source.indexOf('export async function backupHost('));
    if(!body.startsWith('async function snapshotJournal('))throw Error('Original private snapshot span missing');
    const {createRequire}=await import('node:module');const require=createRequire(import.meta.url);
    const ts=require(process.env.IVY_TEST_TYPESCRIPT_MODULE??'typescript');
    const js=ts.transpileModule(body+'\nreturn snapshotJournal;', {compilerOptions:{target:ts.ScriptTarget.ESNext}}).outputText;
    const {open}=await import('node:fs/promises');
    const fn=new Function('DatabaseSync','backup','open','requireThat',js)(DatabaseSync,backup,open,(ok,code,message)=>{if(!ok)throw new IvyError(code,message);});
    process.stdout.write(JSON.stringify(await wrap(()=>fn(input.source,input.destination))));
  }else if(input.kind==='inspectSnapshot'){
    const db=new DatabaseSync(input.source,{readOnly:true});
    try{
      const schema=db.prepare('SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY name,type').all();
      const tables=schema.filter(row=>row.type==='table').map(row=>{
        const query=db.prepare('SELECT * FROM "'+row.name.replaceAll('"','""')+'"');
        query.setReadBigInts(true);query.setReturnArrays(true);
        const rows=query.all().map(row=>row.map(value=>typeof value==='bigint'?{integer:String(value)}:value instanceof Uint8Array?{blob:Buffer.from(value).toString('hex')}:value));
        rows.sort((a,b)=>JSON.stringify(a).localeCompare(JSON.stringify(b)));
        return {name:row.name,rows};
      });
      process.stdout.write(JSON.stringify({schema,tables,userVersion:db.prepare('PRAGMA user_version').get().user_version,quickCheck:db.prepare('PRAGMA quick_check').all()}));
    }finally{db.close();}
  }else if(input.kind==='project'){
    const {projectStorageAreas}=await import('../../packages/host-runtime/src/project-storage.ts');
    process.stdout.write(JSON.stringify(await wrap(()=>projectStorageAreas(input.config))));
  }else if(input.kind==='owners'){
    const {offlineOwners}=await import('../../packages/host-runtime/src/private-backup.ts');
    process.stdout.write(JSON.stringify(await wrap(async()=>{const close=await offlineOwners(input.config);close();})));
  }else if(input.kind==='verify'){
    const {verifyBackup}=await import('../../packages/host-runtime/src/restore.ts');
    process.stdout.write(JSON.stringify(await wrap(()=>verifyBackup(input.destination,input.manifestHash))));
  }else if(input.kind==='backup'){
    const {backupHost}=await import('../../packages/host-runtime/src/private-backup.ts');
    process.stdout.write(JSON.stringify(await wrap(()=>backupHost(input.configPath,input.destination,input.distributionRoot))));
  }else throw Error('Unknown backup fixture operation');
}
