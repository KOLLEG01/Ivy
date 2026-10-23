import {existsSync,readFileSync,readdirSync,lstatSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {join,relative} from 'node:path';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}return next(specifier,context);}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const source=readFileSync(new URL('../../packages/host-runtime/src/config.ts',import.meta.url),'utf8');
const start=source.indexOf('export async function atomicJson('),end=source.indexOf('\nexport function inside(',start);
if(start<0||end<start)throw Error('Original atomic writer boundary changed');
const module=`
import {mkdir,open,unlink} from 'node:fs/promises';
import {dirname} from 'node:path';
import {randomUUID} from 'node:crypto';
import {canonical} from '${new URL('../../packages/contracts/src/canonical.ts',import.meta.url)}';
import {replaceFile} from '${new URL('../../packages/host-runtime/src/atomic-file.ts',import.meta.url)}';
${source.slice(start,end)}
`;
const {atomicJson}=await import('data:text/javascript,'+encodeURIComponent(stripTypeScriptTypes(module,{mode:'transform'})));
const input=JSON.parse(readFileSync(0,'utf8'));
function value(){
  if(input.kind==='unsafe')return {extra:9007199254740992};
  if(input.kind==='infinite')return {extra:Infinity};
  if(input.kind==='nan')return {extra:NaN};
  if(input.kind==='deep'){let value=null;for(let i=0;i<130;i++)value=[value];return value;}
  return {z:'last',a:'ü 空 😀',nested:{b:false,a:null}};
}
function tree(root){
  const rows=[];
  function walk(directory){for(const name of readdirSync(directory)){
    const file=join(directory,name),metadata=lstatSync(file);
    let path=relative(root,file).replaceAll('\\','/');
    path=path.replace(/\.[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}\.tmp$/,'.<uuid>.tmp');
    if(metadata.isDirectory()){rows.push({path,kind:'directory'});walk(file);}
    else{const bytes=readFileSync(file);rows.push({path,kind:'file',bytes:bytes.length,hash:'sha256:'+createHash('sha256').update(bytes).digest('hex')});}
  }}walk(root);return rows.sort((a,b)=>a.path<b.path?-1:a.path>b.path?1:0);
}
let operation;
try{await atomicJson(input.path,value());operation={ok:true,value:null};}
catch(error){operation={ok:false,error:IvyError.from(error).toWire()};}
process.stdout.write(JSON.stringify({operation,tree:tree(input.root)}));
