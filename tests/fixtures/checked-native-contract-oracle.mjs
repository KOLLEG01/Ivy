import './schema-oracle-dependencies.mjs';
import {readFileSync,writeFileSync,existsSync,readdirSync,mkdtempSync,mkdirSync,rmSync,copyFileSync} from 'node:fs';
import {resolve,join,dirname} from 'node:path';import {tmpdir} from 'node:os';
import {registerHooks} from 'node:module';import {fileURLToPath,pathToFileURL} from 'node:url';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const {nativeVersions}=await import('../../packages/contracts/src/native-versions.ts');
const original=readFileSync('packages/contracts/src/checked-native-contract.ts','utf8').replace(/from '(\.\/[^']+\.js)'/g,(_,p)=>'from '+JSON.stringify(pathToFileURL(resolve('packages/contracts/src',p.replace(/\.js$/,'.ts'))).href));
const base=JSON.parse(readFileSync('specs/native/codex-0.159.2/catalog.json','utf8'));
const thin={...base,clientRequests:[{...base.clientRequests[0],inputSchema:true,outputSchema:true}],serverRequests:[],serverNotifications:[],clientNotifications:[],derivedLegacyTypes:[]};
const alternate={...thin,sourceHash:'sha256:'+'a'.repeat(64)};
const zero='sha256:'+'0'.repeat(64),version=thin.version;
const directory='specs/native/codex-'+version,defaultFile=directory+'/catalog.json',platformFile=directory+'/catalog.'+process.platform+'-'+process.arch+'.json';
const write=(path,value)=>({kind:'write',path,bytes:[...Buffer.from(typeof value==='string'?value:JSON.stringify(value))]});
const read=(identity,selectedVersion=version)=>({kind:'read',version:JSON.stringify(selectedVersion),...(identity===undefined?{}:{identity:JSON.stringify(identity)})});
const rows=[];
async function collect(label,ops){const root=mkdtempSync(join(tmpdir(),'ivy-checked-oracle-')),module=join(root,'packages/contracts/src/checked-native-contract.ts');mkdirSync(dirname(module),{recursive:true});writeFileSync(module,original);
 try{const {checkedNativeContract}=await import(pathToFileURL(module).href);
  for(const op of ops){if(op.kind==='write'){mkdirSync(dirname(join(root,op.path)),{recursive:true});writeFileSync(join(root,op.path),Buffer.from(op.bytes));}
   else if(op.kind==='copy'){mkdirSync(dirname(join(root,op.path)),{recursive:true});copyFileSync(op.source,join(root,op.path));}
   else if(op.kind==='mkdir')mkdirSync(join(root,op.path),{recursive:true});
   else if(op.kind==='remove')rmSync(join(root,op.path),{recursive:true,force:true});
   else {try{const selected=checkedNativeContract(JSON.parse(op.version),op.identity===undefined?undefined:JSON.parse(op.identity));const {catalog,contract}=selected;op.expected=JSON.stringify({ok:true,value:{catalogHash:selected.catalogHash,version:catalog.version,sourceHash:catalog.sourceHash,nativeExecutableHash:catalog.nativeExecutableHash,definitions:[...contract.definitions().keys()]}});}catch(e){op.expected=JSON.stringify({ok:false,error:IvyError.from(e).toWire()});}}
  }
  rows.push({label,ops});
 }finally{rmSync(root,{recursive:true,force:true});}
}
const retained=[];
for(const dir of readdirSync('specs/native').filter(s=>/^codex-/.test(s)))for(const file of readdirSync('specs/native/'+dir).filter(s=>/^catalog(?:\.[a-z0-9]+-[a-z0-9]+)?\.json$/.test(s))){const path='specs/native/'+dir+'/'+file;retained.push({path,catalog:JSON.parse(readFileSync(path,'utf8'))});}
await collect('all-retained',[...retained.map(({path})=>({kind:'copy',source:path,path})),...nativeVersions.map(v=>read(undefined,v)),...retained.flatMap(({catalog:c})=>[read({catalogHash:hashJson(c)},c.version),read({sourceHash:c.sourceHash,nativeExecutableHash:c.nativeExecutableHash},c.version)])]);
await collect('cache-and-null',[write(defaultFile,thin),read(null),read(),read(null),write(defaultFile,alternate),read(),read({catalogHash:hashJson(alternate)}),read({catalogHash:hashJson(thin)}),write(defaultFile,thin),read({catalogHash:hashJson(thin)})]);
await collect('identity-order',[write(defaultFile,thin),read({sourceHash:thin.sourceHash,nativeExecutableHash:thin.nativeExecutableHash}),read({nativeExecutableHash:thin.nativeExecutableHash,sourceHash:thin.sourceHash}),read({sourceHash:thin.sourceHash,nativeExecutableHash:thin.nativeExecutableHash})]);
await collect('platform-default',[write(defaultFile,thin),write(platformFile,alternate),read(),{kind:'remove',path:platformFile},read(),read({catalogHash:hashJson(thin)})]);
await collect('missing-retry',[read(),read({catalogHash:zero}),write(defaultFile,thin),read(),read({catalogHash:hashJson(thin)})]);
await collect('empty-directory',[{kind:'mkdir',path:directory},read({catalogHash:zero}),read()]);
await collect('directory-file',[write(directory,'file'),read(),read({catalogHash:zero})]);
await collect('selected-directory',[write(defaultFile,thin),{kind:'mkdir',path:platformFile},read(),read({catalogHash:hashJson(thin)})]);
await collect('ignore-names',[write(defaultFile,thin),...['catalog.UPPER-x64.json','catalog.x-y-z.json','catalog.x_.json','catalog.x-y.json.bak'].map(name=>write(directory+'/'+name,'broken')),read({catalogHash:hashJson(thin)})]);
await collect('first-match',[write(directory+'/catalog.aaa-x64.json',alternate),write(defaultFile,thin),read({nativeExecutableHash:thin.nativeExecutableHash}),read()]);
await collect('first-error',[write(directory+'/catalog.aaa-x64.json','broken'),write(defaultFile,thin),read({catalogHash:hashJson(thin)}),{kind:'remove',path:directory+'/catalog.aaa-x64.json'},read({catalogHash:hashJson(thin)})]);
for(const identity of [{},{catalogHash:zero},{catalogHash:'SHA256:'+'a'.repeat(64)},{catalogHash:'sha256:'+'A'.repeat(64)},{catalogHash:zero+'\n'},{catalogHash:null},{catalogHash:1},{catalogHash:true},{catalogHash:zero,extra:zero},[],[zero],'',zero,0,true,false,null])await collect('invalid-identity',[write(defaultFile,thin),read(identity)]);
for(const v of ['', '0.159.3','0.159.02','../0.159.2','\ud800'])await collect('unsupported-version',[read(null,v),read(undefined,v)]);
for(const source of ['{','\ufeff'+JSON.stringify(thin),'null','[]','{}',JSON.stringify({...thin,version:'0.154.0'}),JSON.stringify({...thin,clientRequests:[thin.clientRequests[0],thin.clientRequests[0]]})])await collect('invalid-file',[write(defaultFile,source),read(),read({catalogHash:zero})]);
const malformed=Buffer.from(JSON.stringify(thin));const marker=Buffer.from(thin.clientRequests[0].method);const index=malformed.indexOf(marker);malformed[index]=0xff;
await collect('utf8-replacement',[{kind:'write',path:defaultFile,bytes:[...malformed]},read(),read({nativeExecutableHash:thin.nativeExecutableHash})]);
process.stdout.write(JSON.stringify({versions:JSON.stringify(nativeVersions),rows}));
