import {existsSync,readFileSync,readdirSync,lstatSync,readlinkSync,statSync} from 'node:fs';
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
function span(source,start,end){const first=source.indexOf(start),last=source.indexOf(end,first+start.length);if(first<0||last<first)throw Error('Original helper boundary changed');return source.slice(first,last);}
const source=`
import {mkdir,cp,stat,realpath,readdir,rm,symlink,link,copyFile} from 'node:fs/promises';
import {join,relative,dirname,isAbsolute,resolve} from 'node:path';
import {mapConcurrent} from '${new URL('../../packages/host-runtime/src/concurrency.ts',import.meta.url)}';
import {requireThat} from '${new URL('../../packages/contracts/src/errors.ts',import.meta.url)}';
${span(config,'export function inside(','\nexport async function instanceConfig(')}
${span(prepare,'async function linkDependencyDirectory(','\ntype WorkspaceLink =')}
${span(prepare,'async function restoreWorkspaceLinks(','\nasync function restoreDependencies(')}
export {linkDependencyDirectory,restoreWorkspaceLinks};
`;
const helpers=await import('data:text/javascript,'+encodeURIComponent(stripTypeScriptTypes(source,{mode:'transform'})));
const input=JSON.parse(readFileSync(0,'utf8'));
function tree(root,source){
  if(!existsSync(root))return null;
  const rows=[];
  function walk(directory,local){for(const name of readdirSync(directory).sort()){
    const file=path.join(directory,name),entry=lstatSync(file),relative=local?local+'/'+name:name;
    if(entry.isSymbolicLink())rows.push({path:relative,type:'link',target:readlinkSync(file).replaceAll(root,'$workspace')});
    else if(entry.isDirectory()){rows.push({path:relative,type:'directory',...(process.platform==='win32'?{}:{mode:entry.mode&0o777})});walk(file,relative);}
    else {
      let hardlink=false;
      if(source){try{const original=statSync(path.join(source,...relative.split('/')),{bigint:true}),actual=statSync(file,{bigint:true});hardlink=actual.dev===original.dev&&actual.ino===original.ino;}catch{}}
      rows.push({path:relative,type:'file',hex:readFileSync(file).toString('hex'),hardlink,...(process.platform==='win32'?{}:{mode:entry.mode&0o777})});
    }
  }}
  walk(root,'');return rows;
}
let operation;
try{
  if(input.operation==='directory')await helpers.linkDependencyDirectory(input.source,input.original);
  else if(input.operation==='workspace')await helpers.restoreWorkspaceLinks(input.original,new Map(input.links));
  else throw Error('Unknown helper operation');
  operation={ok:true,value:null};
}catch(error){operation={ok:false,error:IvyError.from(error).toWire()};}
const sourceRoot=input.operation==='directory'?input.source:null;
process.stdout.write(JSON.stringify({operation,actualTree:tree(input.actual,sourceRoot),originalTree:tree(input.original,sourceRoot)}));
