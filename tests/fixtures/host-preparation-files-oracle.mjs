import {existsSync,readFileSync,readdirSync,lstatSync,readlinkSync} from 'node:fs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
import path from 'node:path';

registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const prepare=readFileSync(new URL('../../packages/host-runtime/src/prepare.ts',import.meta.url),'utf8');
const config=readFileSync(new URL('../../packages/host-runtime/src/config.ts',import.meta.url),'utf8');
function span(source,start,end){
  const first=source.indexOf(start),last=source.indexOf(end,first+start.length);
  if(first<0||last<first)throw Error('Original preparation helper boundary changed');
  return source.slice(first,last);
}
// Evaluate exact original helper bodies and inline collection loops, avoiding
// unrelated journal/validator/parser imports. No copy algorithm is reproduced.
const moduleSource=`
import {mkdir,cp,stat,readFile,realpath,rename,copyFile,readdir,rm,symlink,link,chmod,writeFile} from 'node:fs/promises';
import {join,resolve,relative,dirname,isAbsolute} from 'node:path';
import {mapConcurrent} from '${new URL('../../packages/host-runtime/src/concurrency.ts',import.meta.url)}';
import {requireThat} from '${new URL('../../packages/contracts/src/errors.ts',import.meta.url)}';
${span(config,'export function inside(', '\nexport async function instanceConfig(')}
${span(prepare,'async function copyDirectory(', '\ntype WorkspaceLink =')}
${span(prepare,'const selectedDependencyPath =', '\nfunction lockedPackageName(')}
async function collectServiceFiles(sourceRoot,artifactRoot,runtimePackages,dependencyCacheRoot){
${span(prepare,"      for (const name of ['dist', 'node_modules', 'package.json', 'package-lock.json']) {",'      await materializePortableDependencies(')}
}
async function collectAppBundle(sourceRoot,artifactRoot,dist){const plan={app:{dist}};
${span(prepare,'      const app = plan.app!, sourceBundle =', '\n    } else {')}
}
export {copySourceTree,copyRuntimeDependencies,collectServiceFiles,collectAppBundle};
`;
const helpers=await import('data:text/javascript,'+encodeURIComponent(stripTypeScriptTypes(moduleSource,{mode:'transform'})));
function tree(root){
  if(!existsSync(root))return null;
  const files=[];
  function walk(directory,local){
    for(const name of readdirSync(directory).sort()){
      const file=path.join(directory,name),entry=lstatSync(file),relative=local?local+'/'+name:name;
      if(entry.isSymbolicLink())files.push({path:relative,type:'link',target:readlinkSync(file)});
      else if(entry.isDirectory()){files.push({path:relative,type:'directory',...(process.platform==='win32'?{}:{mode:entry.mode&0o777})});walk(file,relative);}
      else files.push({path:relative,type:'file',hex:readFileSync(file).toString('hex'),...(process.platform==='win32'?{}:{mode:entry.mode&0o777})});
    }
  }
  walk(root,'');return files;
}
const input=JSON.parse(readFileSync(0,'utf8'));
let operation;
try{
  if(input.operation==='source')await helpers.copySourceTree(input.source,input.destination);
  else if(input.operation==='runtime')await helpers.copyRuntimeDependencies(input.source,input.destination,new Set(input.selected),input.cache);
  else if(input.operation==='service')await helpers.collectServiceFiles(input.source,input.destination,new Set(input.selected),input.cache);
  else if(input.operation==='app')await helpers.collectAppBundle(input.source,input.destination,input.dist);
  else throw Error('Unknown preparation-files operation');
  operation={ok:true,value:null};
}catch(error){operation={ok:false,error:IvyError.from(error).toWire()};}
// Failure can stop admission at different members of the same concurrent batch.
// Compare its error/containment, retaining partial trees separately in the corpus.
process.stdout.write(JSON.stringify({ok:true,value:{operation,tree:operation.ok?tree(input.destination):null}}));
