import assert from 'node:assert/strict';
import {DatabaseSync} from 'node:sqlite';
import * as fs from 'node:fs/promises';
import {readFileSync} from 'node:fs';
import {dirname,join,relative,sep} from 'node:path';
import {stripTypeScriptTypes} from 'node:module';
import {pathToFileURL} from 'node:url';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {fileHash} from '../../dist/packages/host-runtime/src/artifact.js';
import {hashJson} from '../../dist/packages/contracts/src/canonical.js';

// Reuse the real native-recovery fixture and actual original function. Only
// import locations change; storage, home, validation and copy bodies stay intact.
export async function runOuter({input,parent,fixture,guard,copyVerified,exists,inventoryFiles,backupPath,original}){
 const source=readFileSync('packages/host-runtime/src/native-restore.ts','utf8');
 const base=new URL('../../dist/packages/host-runtime/src/',import.meta.url);
 const module=stripTypeScriptTypes(source,{mode:'transform'}).replace(/from\s+(['"])(\.{1,2}\/[^'"]+)\1/g,
  (_match,q,s)=>'from '+q+(s==='./backup-files.js'?pathToFileURL(backupPath).href:new URL(s,base).href)+q);
 const path=join(parent,'original-native-restore.mjs');await guard(path);
 try{await fs.writeFile(path,module,{flag:'wx'});}catch(error){if(error.code!=='EEXIST')throw error;assert.equal(await fs.readFile(path,'utf8'),module);}
 const {relocateNativeStorage}=await import(pathToFileURL(path).href);
 const saved=join(parent,'outer-restore-cases.json');
 const names=['0.154.0','0.158.0','0.159.2','restored-none','backup-mismatch','prior-missing','prior-component',
  'external-shared','implicit-shared','new-home-absent','relative-home','build-mismatch','version-mismatch',
  'unsupported-database','zero-alternate','orphan-companion','absent-metadata','unknown-version','path-version',
  'layout-version','layout-schema','platform-preferred','changed-base','changed-rollout','existing-before-image'];
 async function copyTree(from,to){for(const file of await inventoryFiles(from)){
  const dest=join(to,file.path);await guard(dest);await copyVerified(join(from,file.path),dest,file,true);
 }}
 async function observation(row){
  assert.deepEqual(await inventoryFiles(row.original.runtimeRoot),row.protected.source,'Source fixture changed');
  assert.deepEqual(await inventoryFiles(row.backupRoot),row.protected.backup,'Backup fixture changed');
  const reportPath=join(row.target.runtimeRoot,'native-recovery.json');let report=null;
  if(await exists(reportPath)){report=JSON.parse(await fs.readFile(reportPath,'utf8'));assert.match(report.completedAt,/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/);}
  const database=join(row.newHome,'state_5.sqlite');let data=null,workingHash=null;
  if(await exists(database)){
   let db;try{db=new DatabaseSync(database,{readOnly:true});data={ok:true,value:original.digest(db)};}
   catch(error){data={ok:false,error:IvyError.from(error).toWire()};}finally{db?.close();}
   workingHash=await fileHash(database);
  }
  const local=relative(row.target.runtimeRoot,database).split(sep).join('/');
  const inventory=await inventoryFiles(row.target.runtimeRoot);
  const files=inventory.filter(file=>file.path!=='native-recovery.json'&&!['','-wal','-shm'].some(suffix=>file.path===local+suffix));
  return{report,data,workingHash,files,sourcePreserved:true,backupPreserved:true};
 }
 if(input.action==='outer-prepare'){
  const cases=[];
  for(const name of names){
   const version=name.startsWith('0.')?name:'0.154.0',f=await fixture({after(){}},version);
   const row={name,root:f.root,original:f.original,target:f.target,manifest:f.manifest,newHome:f.newHome,oldHome:f.oldHome,backupRoot:f.backupRoot};
   row.distributionRoot=join(f.root,'distribution');const directory=join(row.distributionRoot,'dist/specs/native','codex-'+version);
   await guard(directory);await fs.mkdir(directory,{recursive:true});await fs.writeFile(join(directory,'storage.json'),JSON.stringify(f.layout));
   if(name==='restored-none')delete row.target.restoredFrom;
   if(name==='backup-mismatch')row.target.restoredFrom.backupId='another-backup';
   if(name==='prior-missing')row.original.instances=[];
   if(name==='prior-component')row.original.instances[0].componentId='unrelated';
   if(name==='external-shared')row.original.instances[0].settings.appServer={mode:'external-proxy',expectedCodexHome:join(f.root,'shared-home')};
   if(name==='implicit-shared'){delete row.original.instances[0].settings.nativeHome;delete row.target.instances[0].settings.nativeHome;}
   if(name==='new-home-absent')delete row.target.instances[0].settings.nativeHome;
   if(name==='relative-home')row.target.instances[0].settings.nativeHome=join(f.newHome,'different');
   if(name==='build-mismatch')row.target.instances[0].settings.nativeExecutableHash=hashJson('another-build');
   if(name==='version-mismatch')row.target.instances[0].settings.nativeVersion='0.150.0';
   if(['unsupported-database','zero-alternate'].includes(name))row.manifest.files.push({path:'state/instances/agent/data/native-home/state_6.sqlite',bytes:name==='zero-alternate'?0:4,mode:384,hash:hashJson('future')});
   if(['orphan-companion','absent-metadata'].includes(name)){
    for(const suffix of ['','-wal','-shm']){const file=join(f.newHome,'state_5.sqlite'+suffix);if(await exists(file))await fs.unlink(file);}
    row.manifest.files=row.manifest.files.filter(file=>!/\/state_5\.sqlite(?:-wal|-shm)?$/.test(file.path));
    if(name==='orphan-companion')row.manifest.files.push({path:'state/instances/agent/data/native-home/state_5.sqlite-wal',bytes:4,mode:384,hash:hashJson('orphan')});
   }
   if(['unknown-version','path-version'].includes(name))for(const c of[row.original,row.target])c.instances[0].settings.nativeVersion=name==='unknown-version'?'0.999.0':'../codex-0.154.0';
   if(name==='layout-version')await fs.writeFile(join(directory,'storage.json'),JSON.stringify({...f.layout,nativeVersion:'0.999.0'}));
   if(name==='layout-schema')await fs.writeFile(join(directory,'storage.json'),JSON.stringify({...f.layout,schemaVersion:2}));
   if(name==='platform-preferred'){
    await fs.writeFile(join(directory,'storage.json'),JSON.stringify({...f.layout,nativeVersion:'0.999.0'}));
    await fs.writeFile(join(directory,'storage.'+process.platform+'-'+process.arch+'.json'),JSON.stringify(f.layout));
   }
   if(name==='changed-base')await fs.appendFile(join(f.newHome,'state_5.sqlite'),'changed');
   if(name==='changed-rollout')await fs.writeFile(join(f.newHome,'sessions/paginated-selected.jsonl'),'changed');
   if(name==='existing-before-image'){
    const file=join(row.target.runtimeRoot,'recovery-original/native/agent/state_5.sqlite');await fs.mkdir(dirname(file),{recursive:true});await fs.writeFile(file,'retained');
   }
   row.template=join(f.root,'copied-outer-template');await copyTree(row.target.runtimeRoot,row.template);
   row.protected={source:await inventoryFiles(row.original.runtimeRoot),backup:await inventoryFiles(row.backupRoot)};
   cases.push(row);
  }
  await fs.writeFile(saved,JSON.stringify(cases));console.log(JSON.stringify(cases));return;
 }
 const cases=JSON.parse(await fs.readFile(saved,'utf8')),rows=[];
 for(const row of cases){
  if(input.action==='outer-reset'){
   const path=row.target.runtimeRoot;await guard(path);assert.equal(await fs.realpath(path),path);assert(!(await fs.lstat(path)).isSymbolicLink());
   await fs.rm(path,{recursive:true,force:true});await copyTree(row.template,path);
  }else if(input.action==='outer-original'){
   let reply;try{reply={ok:true,value:await relocateNativeStorage(row.original,row.target,row.manifest,row.distributionRoot)};}
   catch(error){reply={ok:false,error:IvyError.from(error).toWire()};}
   rows.push({name:row.name,reply,...await observation(row)});
  }else if(input.action==='outer-inspect')rows.push({name:row.name,...await observation(row)});
  else throw Error('Unknown outer fixture action');
 }
 console.log(JSON.stringify(input.action==='outer-reset'?{reset:true,cases:cases.length}:rows));
}
