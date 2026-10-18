import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync,statSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {DatabaseSync} from 'node:sqlite';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
},load(url,context,next){
  if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
  return next(url,context);
}});
const {HiveStore}=await import('../../services/hive/src/store.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const input=JSON.parse(readFileSync(0,'utf8'));
const deadline=setTimeout(()=>{console.error('Original online-backup fixture exceeded its bound.');process.exit(72);},20000);
const digest=bytes=>'sha256:'+createHash('sha256').update(bytes).digest('hex');
function inspect(path){
  if(!existsSync(path))return{exists:false};
  if(statSync(path).isDirectory())return{exists:true,directory:true};
  const bytes=readFileSync(path);
  let db;
  try{
    db=new DatabaseSync(path,{readOnly:true});
    const tables=new Set(db.prepare("SELECT name FROM sqlite_schema WHERE type='table'").all().map(row=>row.name));
    const result={exists:true,bytes:bytes.length,digest:digest(bytes),pages:db.prepare('PRAGMA page_count').get().page_count,check:db.prepare('PRAGMA quick_check').get().quick_check};
    if(tables.has('sentinel'))result.sentinel=db.prepare('SELECT value FROM sentinel').all();
    if(tables.has('fixture_payload')){
      const hash=createHash('sha256');let count=0;
      for(const row of db.prepare('SELECT id,bytes FROM fixture_payload ORDER BY id').iterate()){
        hash.update(String(row.id)+'\0');hash.update(row.bytes);count++;
      }
      result.payload={count,hash:'sha256:'+hash.digest('hex')};
    }
    if(tables.has('credentials'))result.credentials=db.prepare('SELECT principal_id,digest,revoked FROM credentials ORDER BY digest').all();
    if(tables.has('metadata'))result.probe=db.prepare("SELECT value FROM metadata WHERE key='probe'").get()?.value??null;
    return result;
  }catch{return{exists:true,bytes:bytes.length,digest:digest(bytes),invalid:true};}
  finally{db?.close();}
}
const results=[];
for(const item of input.cases??[]){
  const destination=item.units?String.fromCharCode(...item.units):item.destination;
  let store,locker,value=null,error=null,progress=0;
  try{
    store=new HiveStore(item.source);
    if(item.sourceLocked)store.db.exec('BEGIN IMMEDIATE');
    if(item.targetLocked){locker=new DatabaseSync(destination);locker.exec('BEGIN IMMEDIATE');}
    value=await store.backupTo(destination,()=>{
      progress++;
      if(item.mutate&&progress===1){store.configureCredentials([]);store.setMetadata('probe','after');}
      if(item.transientLock&&progress===1){store.db.exec('BEGIN IMMEDIATE');setImmediate(()=>store.db.exec('ROLLBACK'));}
      if(progress===item.throwAt)throw new IvyError('fixture_backup_abort','Backup callback requested failure.','completed',{phase:item.phase});
    });
  }catch(e){error=IvyError.from(e).toWire();}
  finally{
    if(item.sourceLocked)store?.db.exec('ROLLBACK');
    if(locker){locker.exec('ROLLBACK');locker.close();}
    store?.close();
  }
  results.push({value,error,progress,state:inspect(item.inspect??destination)});
}
clearTimeout(deadline);
const versionDb=new DatabaseSync(':memory:');
const sqlite=versionDb.prepare('SELECT sqlite_version() AS version').get().version;
versionDb.close();
process.stdout.write(JSON.stringify({results,verified:(input.verify??[]).map(inspect),sqlite}));
