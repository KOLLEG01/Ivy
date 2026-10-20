import assert from 'node:assert/strict';
import {DatabaseSync} from 'node:sqlite';
import {createHash} from 'node:crypto';
import * as fs from 'node:fs/promises';
import {readFileSync} from 'node:fs';
import {dirname,join,relative,resolve,isAbsolute,sep} from 'node:path';
import {stripTypeScriptTypes} from 'node:module';
import {pathToFileURL} from 'node:url';
import {canonical,hashJson} from '../../dist/packages/contracts/src/canonical.js';
import {IvyError,requireThat} from '../../dist/packages/contracts/src/errors.js';
import {jsonFile,inside} from '../../dist/packages/host-runtime/src/config.js';
import {fileHash} from '../../dist/packages/host-runtime/src/artifact.js';

const input=JSON.parse(readFileSync(0,'utf8')),parent=await fs.realpath(input.root);
const within=(root,path)=>{const local=relative(root,resolve(path));return local!==''&&local!=='..'&&!local.startsWith('..'+sep)&&!isAbsolute(local);};
assert(within(await fs.realpath(process.env.IVY_TEST_TEMP??process.env.TEMP??process.env.TMPDIR),parent));
assert(!((await fs.lstat(input.root)).isSymbolicLink()));
async function guard(path){
 assert(within(parent,path),'Fixture operation leaves its explicit temporary root');
 let ancestor=resolve(path);
 while(true){try{const actual=await fs.realpath(ancestor);assert(actual===parent||within(parent,actual),'Fixture ancestor leaves its real temporary root');break;}catch(error){if(error.code!=='ENOENT')throw error;ancestor=dirname(ancestor);}}
}
// The Agent backend build does not include these Host-only fixture helpers.
// Load their actual source, changing only relative module specifiers to the
// already-built shared modules. The helper body is not reimplemented here.
const backupSource=readFileSync('packages/host-runtime/src/backup-files.ts','utf8');
const backupBase=new URL('../../dist/packages/host-runtime/src/',import.meta.url);
const backupModule=stripTypeScriptTypes(backupSource,{mode:'transform'}).replace(/from\s+(['"])(\.{1,2}\/[^'"]+)\1/g,(_match,quote,specifier)=>'from '+quote+new URL(specifier,backupBase).href+quote);
const backupPath=join(parent,'original-backup-files.mjs');await guard(backupPath);
try{await fs.writeFile(backupPath,backupModule,{flag:'wx'});}catch(error){if(error.code!=='EEXIST')throw error;assert.equal(await fs.readFile(backupPath,'utf8'),backupModule);}
const {copyVerified,exists,inventoryFiles,relativeFile}=await import(pathToFileURL(backupPath).href);
const source=readFileSync('packages/host-runtime/src/native-restore.ts','utf8');
const helpers=source.slice(source.indexOf('const sqlName'),source.indexOf('/** Called only'));
const start=source.indexOf('    const database = join(newHome'),end=source.indexOf('    report.instances.push(',start);
assert(start>0&&end>start);
const original=new Function('DatabaseSync','createHash','lstat','open','realpath','isAbsolute','join','relative','sep','inside','relativeFile','fileHash','exists','canonical','hashJson','requireThat',
 stripTypeScriptTypes(helpers,{mode:'transform'})+'\nreturn {map:mappedFile,run:async function(input){const {newHome,oldHome,prefix,layout}=input;const files=new Map(input.manifest.files.map(file=>[file.path,file]));\n'+stripTypeScriptTypes(source.slice(start,end),{mode:'transform'})+'\nreturn {logicalDataHash,changes,workingHash:await fileHash(database)};}};')
 (DatabaseSync,createHash,fs.lstat,fs.open,fs.realpath,isAbsolute,join,relative,sep,inside,relativeFile,fileHash,exists,canonical,hashJson,requireThat);
const fixtureSource=readFileSync('tests/native-restore.test.ts','utf8');
const fixtureStart=fixtureSource.indexOf('async function fixture('),fixtureEnd=fixtureSource.indexOf('\nfor (const version',fixtureStart);
assert(fixtureStart>0&&fixtureEnd>fixtureStart);
const fixture=new Function('mkdtemp','mkdir','writeFile','readFile','rm','realpath','dirname','join','relative','resolve','sep','tmpdir','DatabaseSync','jsonFile','existsSync','inside','hashJson','copyVerified','inventoryFiles','distribution',
 stripTypeScriptTypes(fixtureSource.slice(fixtureStart,fixtureEnd),{mode:'transform'})+'\nreturn fixture;')
 (async prefix=>{await guard(prefix);return fs.mkdtemp(prefix);},async(path,...args)=>{await guard(path);return fs.mkdir(path,...args);},async(path,...args)=>{await guard(path);return fs.writeFile(path,...args);},fs.readFile,
 async()=>{throw Error('Original fixture cleanup is owned by the Rust test');},fs.realpath,dirname,join,relative,resolve,sep,()=>parent,DatabaseSync,jsonFile,
 (await import('node:fs')).existsSync,inside,hashJson,async(from,to,...args)=>{await guard(from);await guard(to);return copyVerified(from,to,...args);},async root=>{await guard(root);return inventoryFiles(root);},resolve('.'));
const saved=join(parent,'restore-cases.json');
async function snap(root){return inventoryFiles(root);}
async function safeCopy(from,to){await guard(from);await guard(to);for(const file of await inventoryFiles(from))await copyVerified(join(from,file.path),join(to,file.path),file,true);}
async function applyPost(row){
 if(row.name==='missing-target')await fs.unlink(join(row.newHome,'sessions/paginated-selected.jsonl'));
 if(row.name==='changed-target')await fs.writeFile(join(row.newHome,'sessions/paginated-selected.jsonl'),'changed');
 if(row.name==='physical-escape'){
  const sessions=join(row.newHome,'sessions'),external=join(row.root,'held-sessions-'+input.action);
  await guard(sessions);await guard(external);await fs.rename(sessions,external);
  await fs.symlink(external,sessions,process.platform==='win32'?'junction':'dir');
 }
}
const names=['0.154.0','0.158.0','0.159.2','user-version','schema','checksum','migration-failed','migration-unsafe','migration-blob','success-blob','success-unsafe','migration-blob-incomplete','migration-blob-unsafe','migration-number-before-blob','migration-blob-with-number','missing-pointer','outside-pointer','optional-outside','nontext-pointer','unsafe-pointer','blob-pointer','duplicate-target','existing-target','nonpath-trigger','foreign-key','missing-column','typed-values','invalid-utf8','changed-target','missing-target','physical-escape','checkpoint-busy'];
if(input.action==='prepare'){
 const cases=[];
 for(const name of names){
  const version=name.startsWith('0.')?name:'0.154.0',f=await fixture({after(){}},version);
  const row={name,version,root:f.root,newHome:f.newHome,oldHome:f.oldHome,prefix:'state/instances/agent/data/native-home',layout:f.layout,manifest:f.manifest,source:f.original.runtimeRoot,backup:f.backupRoot};
  await guard(row.newHome);assert(within(parent,row.oldHome));
  if(!name.startsWith('0.')){
  const db=new DatabaseSync(join(row.newHome,'state_5.sqlite'));
  try{
   if(name==='user-version')db.exec('PRAGMA user_version=1');
   if(name==='schema')db.exec('CREATE TABLE unexpected(value TEXT)');
   if(name==='checksum')db.exec("UPDATE _sqlx_migrations SET checksum=X'00' WHERE version=1");
   if(name==='migration-failed')db.exec('UPDATE _sqlx_migrations SET success=0 WHERE version=1');
   if(name==='migration-unsafe')db.exec('UPDATE _sqlx_migrations SET version=9007199254740993 WHERE version=1');
   if(['migration-blob','migration-blob-incomplete','migration-blob-unsafe','migration-blob-with-number'].includes(name))db.exec("UPDATE _sqlx_migrations SET description=X'00' WHERE version=1");
   if(name==='success-blob')db.exec("UPDATE _sqlx_migrations SET success=X'00' WHERE version=1");
   if(name==='success-unsafe')db.exec('UPDATE _sqlx_migrations SET success=9007199254740993 WHERE version=1');
   if(name==='migration-blob-incomplete')db.exec('UPDATE _sqlx_migrations SET success=0 WHERE version=2');
   if(name==='migration-blob-unsafe')db.exec('UPDATE _sqlx_migrations SET success=9007199254740993 WHERE version=2');
   if(name==='migration-number-before-blob')db.exec("UPDATE _sqlx_migrations SET version=-1e100 WHERE version=1; UPDATE _sqlx_migrations SET description=X'00' WHERE version=2");
   if(name==='migration-blob-with-number')db.exec('UPDATE _sqlx_migrations SET version=-1e100 WHERE version=1');
   if(name==='missing-pointer')db.prepare("UPDATE threads SET rollout_path=? WHERE id='paginated'").run(join(row.oldHome,'sessions/not-captured.jsonl'));
   if(name==='outside-pointer')db.prepare("UPDATE threads SET rollout_path=? WHERE id='paginated'").run(join(row.root,'external.jsonl'));
   if(['optional-outside','existing-target'].includes(name)){
    db.exec('CREATE TABLE pointer_values(path TEXT);');
    const paths=name==='optional-outside'?['relative/file.csv',join(row.root,'external/input.csv'),join(row.oldHome,'jobs/input.csv'),join(row.oldHome,'jobs/not-created.csv')]:[join(row.oldHome,'jobs/input.csv'),join(row.newHome,'jobs/input.csv')];
    for(const path of paths)db.prepare('INSERT INTO pointer_values VALUES (?)').run(path);
    row.layout.pathColumns.push({table:'pointer_values',column:'path',scope:'optional-owned-file'});
   }
   if(name==='nontext-pointer')db.exec("UPDATE threads SET rollout_path=17 WHERE id='paginated'");
   // A non-STRICT fixture table retains actual non-text pointer types.
   if(['unsafe-pointer','blob-pointer'].includes(name)){
    db.exec('CREATE TABLE pointer_values(path);');db.prepare('INSERT INTO pointer_values VALUES (?)').run(name==='blob-pointer'?Buffer.from([0,255]):9007199254740993n);
    row.layout.pathColumns.push({table:'pointer_values',column:'path',scope:'optional-owned-file'});
   }
   if(name==='duplicate-target')db.prepare("UPDATE threads SET rollout_path=? WHERE id='paginated'").run(row.oldHome.replaceAll('\\','/')+'/sessions//legacy.jsonl');
   if(name==='nonpath-trigger')db.exec("CREATE TRIGGER retained_change AFTER UPDATE OF rollout_path ON threads BEGIN UPDATE threads SET title='changed' WHERE id=NEW.id; END");
   if(name==='foreign-key'){db.exec("PRAGMA foreign_keys=OFF; CREATE TABLE invalid_references(thread_id TEXT REFERENCES threads(id)); INSERT INTO invalid_references VALUES ('missing')");}
   if(name==='missing-column')row.layout.pathColumns[0].column='not_present';
   if(name==='typed-values'){
    db.exec('CREATE TABLE digest_values("10", "2", "ordinary", "0", "01", "__proto__", "constructor", "😀");');
    db.prepare('INSERT INTO digest_values VALUES (?,?,?,?,?,?,?,?)').run(9223372036854775807n,-9223372036854775808n,Buffer.from([0,1,127,255]),null,1.25,'Unicode 😀','retained',-0);
   }
   if(name==='invalid-utf8')db.exec("CREATE TABLE text_values(value TEXT); INSERT INTO text_values VALUES(CAST(X'EDA080FF' AS TEXT))");
   if(['optional-outside','existing-target','foreign-key','unsafe-pointer','blob-pointer','nonpath-trigger','typed-values','invalid-utf8'].includes(name))row.layout.schema=db.prepare("SELECT type,name,tbl_name AS tableName,sql FROM sqlite_master WHERE sql IS NOT NULL AND name <> '_sqlx_migrations' ORDER BY type,name").all().map(value=>({...value,sql:String(value.sql).replaceAll('\r\n','\n')}));
  }finally{db.close();}}
  // Re-seal only the mutated copied SQLite observation; no source or backup changes.
  const copied=await inventoryFiles(join(row.root,'target'));
  row.manifest.files=row.manifest.files.filter(file=>!/^state_5\.sqlite(?:-wal|-shm)?$/.test(file.path.slice((row.prefix+'/').length)));
  for(const file of copied.filter(file=>/^state_5\.sqlite(?:-wal|-shm)?$/.test(file.path.slice('instances/agent/data/native-home/'.length))))row.manifest.files.push({...file,path:'state/'+file.path});
  row.template=join(row.root,'copied-template');await safeCopy(join(row.root,'target'),row.template);
  row.protected={source:await snap(row.source),backup:await snap(row.backup)};
  cases.push(row);
 }
 await fs.writeFile(saved,JSON.stringify(cases));
 console.log(JSON.stringify(cases));
}else{
 const cases=JSON.parse(await fs.readFile(saved,'utf8')),results=[];
 for(const row of cases){
  await guard(row.newHome);assert((await fs.realpath(row.newHome))===resolve(row.newHome));
  if(input.action==='original'){
   await applyPost(row);
   let reader;
   if(row.name==='checkpoint-busy'){reader=new DatabaseSync(join(row.newHome,'state_5.sqlite'));reader.exec('BEGIN');reader.prepare('SELECT count(*) FROM threads').all();}
   try{let reply;try{const value=await original.run(row);reply={ok:true,value};}catch(error){reply={ok:false,error:IvyError.from(error).toWire()};}
    results.push({name:row.name,reply,...await observation(row)});
   }finally{reader?.close();}
  }else if(input.action==='reset'){
   const target=join(row.root,'target');await guard(target);assert(within(parent,await fs.realpath(target)));assert(!(await fs.lstat(target)).isSymbolicLink());
   await fs.rm(target,{recursive:true,force:true});await safeCopy(row.template,target);await applyPost(row);
  }else if(input.action==='inspect')results.push({name:row.name,...await observation(row)});
  else if(input.action==='mappings'){
   // Observe paths from a separate SQLite copy. Node must not recover or
   // checkpoint the fresh working copy before the Rust owner opens it.
   const observationHome=join(row.root,'mapping-observation');await guard(observationHome);
   for(const suffix of ['','-wal','-shm']){
    const from=join(row.newHome,'state_5.sqlite'+suffix),to=join(observationHome,'state_5.sqlite'+suffix);
    if(await exists(from)){await guard(from);await guard(to);await copyVerified(from,to);}
   }
   const mappings=[],db=new DatabaseSync(join(observationHome,'state_5.sqlite'),{readOnly:true});
   try{for(const definition of row.layout.pathColumns){
    const quote=name=>'"'+name.replaceAll('"','""')+'"';let values;
    try{const statement=db.prepare('SELECT DISTINCT '+quote(definition.column)+' AS path FROM '+quote(definition.table)+' WHERE '+quote(definition.column)+' IS NOT NULL LIMIT 1000001');statement.setReadBigInts(true);values=statement.all();}catch{continue;}
    for(const {path} of values){if(typeof path!=='string')continue;let reply;
     try{reply={ok:true,value:await original.map(path,definition.scope,row.oldHome,row.newHome,row.prefix,new Map(row.manifest.files.map(file=>[file.path,file])))};}catch(error){reply={ok:false,error:IvyError.from(error).toWire()};}
     mappings.push({path,scope:definition.scope,reply});
    }
   }}finally{db.close();}results.push({name:row.name,mappings});
  }
  else throw Error('Unknown oracle action');
 }
 console.log(JSON.stringify(input.action==='reset'?{reset:true,cases:cases.length}:results));
}
async function observation(row){
 let db;let data;
 try{db=new DatabaseSync(join(row.newHome,'state_5.sqlite'),{readOnly:true});
  // Use the actual original digest, including BigInt/blob/object-entry semantics.
  const load=new Function('createHash','canonical','requireThat',stripTypeScriptTypes(helpers.slice(0,helpers.indexOf('async function mappedFile')),{mode:'transform'})+'\nreturn logicalDigest;');
  try{data={ok:true,value:load(createHash,canonical,requireThat)(db)};}catch(error){data={ok:false,error:IvyError.from(error).toWire()};}
 }finally{db?.close();}
 const sourceNow=await snap(row.source),backupNow=await snap(row.backup);
 assert.deepEqual(sourceNow,row.protected.source,'Source fixture changed');assert.deepEqual(backupNow,row.protected.backup,'Backup fixture changed');
 return{data,sourcePreserved:true,backupPreserved:true,workingHash:await fileHash(join(row.newHome,'state_5.sqlite'))};
}
