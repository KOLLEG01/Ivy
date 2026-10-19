import {existsSync,readFileSync,readdirSync,lstatSync,readlinkSync} from 'node:fs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import path from 'node:path';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
}});
const input=JSON.parse(readFileSync(0,'utf8'));
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const prepare=readFileSync(new URL('../../packages/host-runtime/src/prepare.ts',import.meta.url),'utf8');
const config=readFileSync(new URL('../../packages/host-runtime/src/config.ts',import.meta.url),'utf8');
function span(source,start,end){const a=source.indexOf(start),b=source.indexOf(end,a+start.length);if(a<0||b<a)throw Error('Original helper boundary changed');return source.slice(a,b);}
const fallback=prepare.match(/    if \(directory\) await cp\(source, destination, \{ recursive: true \}\);\r?\n    else await copyFile\(source, destination\);/)?.[0];
if(!fallback)throw Error('Original transfer copy phase changed');
const source=`
import {mkdir,cp,stat,realpath,readdir,rm,symlink,link,copyFile,rename,readFile,open,unlink} from 'node:fs/promises';
import {join,relative,dirname,isAbsolute,resolve} from 'node:path';
import {randomUUID} from 'node:crypto';
import {mapConcurrent} from '${new URL('../../packages/host-runtime/src/concurrency.ts',import.meta.url)}';
import {IvyError,requireThat} from '${new URL('../../packages/contracts/src/errors.ts',import.meta.url)}';
import {canonical,hashJson} from '${new URL('../../packages/contracts/src/canonical.ts',import.meta.url)}';
import {replaceFile} from '${new URL('../../packages/host-runtime/src/atomic-file.ts',import.meta.url)}';
const process=${JSON.stringify(input.profile??{platform:process.platform,arch:process.arch,version:process.version})};
${span(config,'export async function jsonFile<','export async function instanceConfig(')}
${span(prepare,'async function copyDirectory(','async function copySourceTree(')}
${span(prepare,'async function linkDependencyDirectory(','/** Only a different filesystem')}
${span(prepare,'async function transferPreparedPayload(','async function emittedRuntimePackages(')}
async function copyPreparedPayload(source:string,destination:string,directory:boolean):Promise<void>{${fallback}}
export {dependencyCacheKey,workspaceLinkEntries,validCache,restoreDependencies,retainDependencies,copyDirectory,isWorkspacePath,transferPreparedPayload,copyPreparedPayload};
`;
const helpers=await import('data:text/javascript,'+encodeURIComponent(stripTypeScriptTypes(source,{mode:'transform'})));
const links=new Map(input.links??[]);
function tree(root){
  let metadata;try{metadata=lstatSync(root);}catch(error){if(error.code==='ENOENT')return null;throw error;}
  function entry(file,local){
    const metadata=lstatSync(file);
    if(metadata.isSymbolicLink())return [{path:local,type:'link',target:readlinkSync(file).replaceAll(root,'$root')}];
    const permissions=process.platform==='win32'?{readonly:(metadata.mode&0o200)===0}:{mode:metadata.mode&0o777};
    if(metadata.isDirectory())return [{path:local,type:'directory',...permissions},...readdirSync(file).sort().flatMap(name=>entry(path.join(file,name),local?local+'/'+name:name))];
    const bytes=readFileSync(file);
    return [{path:local,type:'file',size:bytes.length,sha256:createHash('sha256').update(bytes).digest('hex'),nlink:metadata.nlink,...permissions}];
  }
  return entry(root,'');
}
let operation,nativeError;
try{
  let value;
  if(input.operation==='key')value=helpers.dependencyCacheKey(input.lockHash,input.packageManager);
  else if(input.operation==='entries')value=helpers.workspaceLinkEntries(links);
  else if(input.operation==='valid')value=helpers.validCache(input.metadata,input.key,links);
  else if(input.operation==='order')value={keys:[...links.keys()],matrix:[...links.keys()].map(left=>[...links.keys()].map(right=>Math.sign(left.localeCompare(right)))),profile:{locale:Intl.DateTimeFormat().resolvedOptions().locale,icu:process.versions.icu,node:process.version,platform:process.platform,arch:process.arch}};
  else if(input.operation==='snapshot')value=null;
  else{
    const sourceRoot=input.sourceOriginal??path.join(input.originalRoot,input.sourceLocal??'source'),cacheRoot=input.destinationOriginal??path.join(input.originalRoot,input.cacheLocal??'cache');
    if(input.operation==='restore')value=await helpers.restoreDependencies(sourceRoot,cacheRoot,input.key,links);
    else if(input.operation==='retain')await helpers.retainDependencies(sourceRoot,cacheRoot,input.key,links);
    else if(input.operation==='copy')await helpers.copyDirectory(path.join(sourceRoot,'node_modules'),cacheRoot,{dereference:false,verbatimSymlinks:true,filter:async file=>!helpers.isWorkspacePath(path.relative(sourceRoot,file).replaceAll('\\','/'),links)});
    else if(input.operation==='transfer')await helpers.transferPreparedPayload(sourceRoot,cacheRoot,input.directory);
    else if(input.operation==='copyPayload')await helpers.copyPreparedPayload(sourceRoot,cacheRoot,input.directory);
    else throw Error('Unknown cache operation');
  }
  operation={ok:true,value:value??null};
}catch(error){nativeError={name:error.name,...(error.code?{code:error.code}:{})};operation={ok:false,error:IvyError.from(error).toWire()};}
process.stdout.write(JSON.stringify({operation,...(nativeError?{nativeError}:{}),...(input.actualRoot?{actualTree:tree(input.actualRoot),originalTree:tree(input.originalRoot)}:{}),...(input.destinationActual?{actualDestinationTree:tree(input.destinationActual),originalDestinationTree:tree(input.destinationOriginal)}:{})}));
