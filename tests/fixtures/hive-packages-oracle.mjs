import {registerHooks,stripTypeScriptTypes,syncBuiltinESMExports} from 'node:module';
import {existsSync,readFileSync,createWriteStream,mkdtempSync,mkdirSync,writeFileSync,rmSync} from 'node:fs';
import {mkdir,rm} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {join,dirname,relative,resolve,sep} from 'node:path';
import {tmpdir} from 'node:os';
import {spawnSync} from 'node:child_process';
import crypto,{createHash} from 'node:crypto';
import {Transform,Readable} from 'node:stream';
import {pipeline} from 'node:stream/promises';
import {setup,tree} from './hive-ui-files-fixture.mjs';
const input=JSON.parse(readFileSync(0,'utf8'));
if(input.dates){process.stdout.write(JSON.stringify(input.dates.map(n=>[n,new Date(n).toISOString()])));process.exit(0);}
if(input.buildArchive){
 const root=mkdtempSync(join(tmpdir(),'ivy-package-fixture-')),manifest=input.buildArchive.manifest;
 try{
  const put=(path,data)=>{mkdirSync(dirname(join(root,path)),{recursive:true});writeFileSync(join(root,path),data);};
  put('component.json',JSON.stringify(manifest));put('artifact/dist/build-info.json',JSON.stringify({schemaVersion:1,componentId:manifest.componentId,version:manifest.version,buildId:manifest.buildId,...input.buildArchive.stamp}));
  if(manifest.kind==='app')put('artifact/'+manifest.app.dist+'/'+manifest.app.entryPath,'<!doctype html><title>Package fixture</title>');
  else{put('artifact/dist/main.js','console.log("fixture");');put('artifact/dist/health.js','console.log("ready");');}
  for(const[path,data]of Object.entries(input.buildArchive.files??{}))put(path,data);
  const tar=process.platform==='win32'?join(process.env.SystemRoot??'C:\\Windows','System32','tar.exe'):'/usr/bin/tar';
  const command=spawnSync(tar,['-czf',join(root,'archive.tar.gz'),'-C',root,'component.json','artifact'],{encoding:'utf8',windowsHide:true});
  if(command.status!==0)throw Error('Fixture tar: '+command.stderr);const bytes=readFileSync(join(root,'archive.tar.gz'));
  process.stdout.write(JSON.stringify({bytesBase64:bytes.toString('base64'),bytes:bytes.length,archiveHash:'sha256:'+createHash('sha256').update(bytes).digest('hex')}));
 }finally{if(!resolve(root).startsWith(resolve(tmpdir())+sep)||!root.includes('ivy-package-fixture-'))throw Error('Unexpected fixture root');rmSync(root,{recursive:true,force:true});}
 process.exit(0);
}
registerHooks({resolve(specifier,context,next){
 if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
 return next(specifier,context);
},load(url,context,next){if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};return next(url,context);}});
const {HiveStore}=await import('../../services/hive/src/store.ts');
const {Objects}=await import('../../services/hive/src/objects.ts');
const {PackageCatalogObject}=await import('../../services/hive/src/package-catalog-object.ts');
const {PackageRegistry}=await import('../../services/hive/src/packages.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {digest}=await import('../../packages/contracts/src/canonical.ts');
const source=stripTypeScriptTypes(readFileSync(new URL('../../services/hive/src/packages.ts',import.meta.url),'utf8'),{mode:'transform'});
const start=source.indexOf('this.collect();',source.indexOf('async upload('));
const paths=source.indexOf('const incoming =',start),archive=source.indexOf('await validatePackageArchive(',paths);
const cleanupStart=source.lastIndexOf('finally{')+'finally{'.length;
let cleanupEnd=cleanupStart,depth=1;for(;cleanupEnd<source.length;cleanupEnd++){if(source[cleanupEnd]==='{')depth++;if(source[cleanupEnd]==='}'&&--depth===0)break;}
if(start<0||paths<start||archive<paths||cleanupStart<archive||cleanupEnd<cleanupStart)throw Error('Original package ingress boundaries changed');
// The guard and ingress cuts execute unchanged source up to the Host archive call.
// They do not stand in for archive validation, installation or catalog publication.
const admit=new Function('request','uploadId','digest','requireThat',source.slice(start,paths)+'return grant;');
const AsyncFunction=Object.getPrototypeOf(async function(){}).constructor;
const cleanup=source.slice(cleanupStart,cleanupEnd);
const stage=new AsyncFunction('request','uploadId','digest','requireThat','join','dirname','mkdir','createHash','Transform','IvyError','pipeline','createWriteStream','rm',
 source.slice(start,archive)+'return {incoming,extraction,grant};\n} catch(error) { '+cleanup+' throw error; }');
const release=new AsyncFunction('incoming','extraction','rm',cleanup);
const common=readFileSync(new URL('./hive-store-oracle.mjs',import.meta.url),'utf8');
const snapshotSource=common.slice(common.indexOf('const tableHash='),common.indexOf('const rows=[];'));
const RealDate=Date,realUUID=crypto.randomUUID,realGlobalUUID=globalThis.crypto.randomUUID,realBytes=crypto.randomBytes;
let store,owner,registry,now=100000,failure=null,installFailure=false;
const requests=[],stages=new Map(),rows=[],installerCalls=[];
const open=()=>{store=new HiveStore(input.path);store.db.exec("UPDATE metadata SET value='epoch' WHERE key='runtime_epoch'; INSERT OR IGNORE INTO principals VALUES('publisher'),('other');");owner=new PackageCatalogObject(store,new Objects(store),()=>new Set());};
const worker={async request(request){
 requests.push(structuredClone(request));
 if(failure&&failure.action===request.action){const e=failure;failure=null;throw new IvyError(e.code,'Fixture worker failure.');}
 switch(request.action){
  case 'package.catalog':return owner.list(request.after,request.includeUis);
  case 'package.seed':return owner.seed(request.catalog,'publisher');
  case 'package.prune':return owner.prune('publisher');
  case 'package.publish':return owner.publish(request.input,request.publisherPrincipalId);
  default:throw Error('Unexpected worker request');
 }
}};
const installUi=async value=>{
 installerCalls.push({manifest:value.manifest,artifactRoot:relative(input.root,value.artifactRoot).replaceAll('\\','/'),publisherPrincipalId:value.publisherPrincipalId,buildId:value.buildId,files:tree(value.artifactRoot)});
 store.db.exec("INSERT INTO metadata VALUES('fixture_ui_installations','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1;");
 if(installFailure)throw new IvyError('fixture_failure','Fixture installer failure.');
};
const construct=(missing,installer=false)=>registry=new PackageRegistry(input.root,input.basePath,{},installer?installUi:undefined,missing?undefined:worker);
const bind=step=>{
 step=structuredClone(step);
 for(const binding of step.bindings??[]){let value=rows[binding.from];for(const key of binding.select.split('/').slice(1))value=value[key];if(binding.prefix)value=binding.prefix+value;if(binding.suffix)value+=binding.suffix;
  let target=step;const keys=binding.at.split('/').slice(1);for(const key of keys.slice(0,-1))target=target[key];target[keys.at(-1)]=value;
 }
 return step;
};
const grantValue=grant=>({authorization:Object.fromEntries(['componentId','version','buildId','archiveHash','bytes','manifest'].map(key=>[key,grant[key]])),principalId:grant.principalId,uploadId:grant.uploadId});
try{
 open();construct(input.missingWorker);
 for(const original of input.steps){
  const step=bind(original);now=step.now??100000;
  globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[now]));}static now(){return now;}};
  let uuid=0,random=0;
  const uuids=step.uuids??[],randoms=step.randoms??[];
  const nextUUID=()=>{if(uuid>=uuids.length)throw Error('Unobserved package UUID');return uuids[uuid++];};
  crypto.randomUUID=nextUUID;globalThis.crypto.randomUUID=nextUUID;
  crypto.randomBytes=size=>{const value=Buffer.from(randoms[random++]??'','base64');if(size!==32||value.length!==32)throw Error('Unobserved package token bytes');return value;};syncBuiltinESMExports();
  let value=null,error=null;
  try{
   switch(step.method){
    case 'fixture':setup(input.root,step.actions);break;
    case 'sql':store.db.exec(step.sql);break;
    case 'failNext':failure=step.failure;break;
    case 'construct':construct(step.missingWorker,step.installer);break;
    case 'installFailure':installFailure=step.enabled;break;
    case 'initialize':await registry.initialize();break;
    case 'catalog':value=await registry.catalog(step.after??0,step.includeUis??true);break;
    case 'snapshot':value=await registry.catalogSnapshot(step.after??0,step.includeUis??true);break;
    case 'authorize':value=await registry.authorize(step.principal??'publisher',step.input);break;
    case 'authorizeMany':value=[];for(let i=0;i<step.count;i++)value.push(await registry.authorize(step.principal??'publisher',step.input));break;
    case 'admit':value=grantValue(admit.call(registry,{headers:step.headers},step.uploadId,digest,requireThat));break;
    case 'stage':{
     const chunks=(step.chunks??[]).map(s=>Buffer.from(s,'base64'));
     const request=Readable.from((async function*(){for(const chunk of chunks)yield chunk;if(step.readFailure)throw Error('Fixture stream failed.');})());
     request.headers=step.headers;request.setTimeout=ms=>{if(ms!==300000)throw Error('Original upload idle timeout changed');};
     const retained=await stage.call(registry,request,step.uploadId,digest,requireThat,join,dirname,mkdir,createHash,Transform,IvyError,pipeline,createWriteStream,rm);
     stages.set(step.label,retained);value={...grantValue(retained.grant),archive:relative(input.root,retained.incoming).replaceAll('\\','/'),extraction:relative(input.root,retained.extraction).replaceAll('\\','/')};break;
    }
    case 'upload':{
     const request=Readable.from((async function*(){for(const chunk of step.chunks??[])yield Buffer.from(chunk,'base64');if(step.readFailure)throw Error('Fixture stream failed.');})());
     request.headers=step.headers;request.setTimeout=ms=>{if(ms!==300000)throw Error('Original upload idle timeout changed');};
     value=await registry.upload(request,step.uploadId);break;
    }
    case 'uploadPair':value=await Promise.all(step.uploads.map(upload=>{
     const request=Readable.from(upload.chunks.map(chunk=>Buffer.from(chunk,'base64')));request.headers=upload.headers;request.setTimeout=ms=>{if(ms!==300000)throw Error('Original upload idle timeout changed');};return registry.upload(request,upload.uploadId);
    }));break;
    case 'dropStage':{const staged=stages.get(step.label);await release(staged.incoming,staged.extraction,rm);stages.delete(step.label);break;}
    case 'artifact':{const artifact=await registry.artifact(step.componentId,step.version);value={path:relative(input.root,artifact.path).replaceAll('\\','/'),entry:artifact.entry};break;}
    case 'collect':await registry.collectArtifacts();break;
    case 'reopen':store.close();open();construct(step.missingWorker,step.installer);break;
    default:throw Error('Unknown package fixture method');
   }
  }catch(e){error=IvyError.from(e).toWire();}
  finally{globalThis.Date=RealDate;crypto.randomUUID=realUUID;globalThis.crypto.randomUUID=realGlobalUUID;crypto.randomBytes=realBytes;syncBuiltinESMExports();}
  const state=new Function('store','createHash',snapshotSource+'return snapshot();')(store,createHash);
  rows.push({value,error,state,fileTree:tree(input.root),requests:structuredClone(requests),installerCalls:structuredClone(installerCalls),catalogMutations:store.all('SELECT mutation_id FROM mutations ORDER BY rowid')});
 }
 process.stdout.write(JSON.stringify({rows,definitions:store.all('SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name')}));
}finally{for(const staged of stages.values())await release(staged.incoming,staged.extraction,rm);store?.close();globalThis.Date=RealDate;crypto.randomUUID=realUUID;globalThis.crypto.randomUUID=realGlobalUUID;crypto.randomBytes=realBytes;syncBuiltinESMExports();}
