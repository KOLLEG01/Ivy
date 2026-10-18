import {existsSync,readFileSync,readdirSync,lstatSync,writeFileSync} from 'node:fs';
import {mkdir,stat,writeFile,rm} from 'node:fs/promises';
import {join,relative} from 'node:path';
import {createHash} from 'node:crypto';
import {gzipSync} from 'node:zlib';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {mapConcurrent}=await import('../../packages/host-runtime/src/concurrency.ts');
const source=readFileSync(new URL('../../packages/host-runtime/src/prepare.ts',import.meta.url),'utf8');
function sourceSpan(source,start,end){const a=source.indexOf(start),b=source.indexOf(end,a+start.length);if(a<0||b<a)throw Error('Original helper boundary changed');return source.slice(a,b);}
const span=(start,end)=>sourceSpan(source,start,end);
const configSource=readFileSync(new URL('../../packages/host-runtime/src/config.ts',import.meta.url),'utf8');
const processSource=readFileSync(new URL('../../packages/host-runtime/src/process.ts',import.meta.url),'utf8');
const processStart=processSource.indexOf('export type Command =');
if(processStart<0)throw Error('Original process import boundary changed');
// Read the exact unrelated-schema-free helpers, retaining the complete original
// process implementation. No dist build or schema/compiler dependency is needed.
const processModule=`
import {spawn} from 'node:child_process';
import {readFile,stat,realpath} from 'node:fs/promises';
import {isAbsolute,resolve,join,relative} from 'node:path';
import {IvyError,requireThat} from '${new URL('../../packages/contracts/src/errors.ts',import.meta.url)}';
${sourceSpan(configSource,'export async function jsonFile','\nexport async function atomicJson')}
${sourceSpan(configSource,'export function inside(','\nexport async function instanceConfig(')}
${processSource.slice(processStart)}
`;
const {runCommand,jsonFile}=await import('data:text/javascript,'+encodeURIComponent(stripTypeScriptTypes(processModule,{mode:'transform'})));
const common=span('function lockedPackageName(','/** Add the supported OS/architecture');
const planner=span('async function materializePortableDependencies(','  await mapConcurrent(missing, 4, async value => {')+'  return missing;\n}';
const policy='async function responsePolicy(value) {\n'+span('    const response = await fetch(value.resolved,','    const key = createHash(\'sha256\').update(value.location)')+'return {byteLength:bytes.length};\n}';
const listing='function listingPolicy(listing) {\n'+span('      const entries = listing.stdout.split(','      await mkdir(destination, { recursive: true });')+'return null;\n}';
const materializer=span('async function materializePortableDependencies(','\nexport async function prepareCandidate(');
// The policy stages are exact original source spans. The response fixture is
// deliberately explicit: these cases do not qualify HTTP, TLS or decompression.
const make=new Function('stat','join','createHash','requireThat','mapConcurrent','fetch','writeFile','runCommand','mkdir','jsonFile','rm',stripTypeScriptTypes(
  common+planner.replace('materializePortableDependencies','dependencyPlan')+policy+listing+materializer,{mode:'transform'})+'\nreturn {dependencyPlan,responsePolicy,listingPolicy,materializePortableDependencies};');
const input=JSON.parse(readFileSync(0,'utf8'));
function body(){if(input.bodyFile)return readFileSync(input.bodyFile);if(input.bodyLength!==undefined)return Buffer.alloc(input.bodyLength,input.fill??97);return Buffer.from(input.bodyBase64??'','base64');}
const fetch=async()=>({ok:input.responseOk!==false,headers:{get:()=>input.declared??null},arrayBuffer:async()=>{const bytes=body();return bytes.buffer.slice(bytes.byteOffset,bytes.byteOffset+bytes.byteLength);}});
const helpers=make(stat,join,createHash,requireThat,mapConcurrent,fetch,writeFile,runCommand,mkdir,jsonFile,rm);
function tree(root){
  if(!root||!existsSync(root))return null;
  const rows=[];function walk(directory){for(const name of readdirSync(directory).sort()){
    const file=join(directory,name),s=lstatSync(file),path=relative(root,file).replaceAll('\\','/');
    if(s.isSymbolicLink())rows.push({path,type:'link'});
    else if(s.isDirectory()){rows.push({path,type:'directory'});walk(file);}
    else rows.push({path,type:'file',sha256:createHash('sha256').update(readFileSync(file)).digest('hex'),bytes:s.size,...(process.platform==='win32'?{}:{mode:s.mode&0o777})});
  }}walk(root);return rows;
}
function entry(name,data,mode=0o644){
  const h=Buffer.alloc(512);h.write(name);h.write(mode.toString(8).padStart(7,'0')+'\0',100);h.write('0000000\0',108);h.write('0000000\0',116);
  h.write(data.length.toString(8).padStart(11,'0')+'\0',124);h.write('00000000000\0',136);h.fill(32,148,156);h[156]=48;h.write('ustar\0',257);h.write('00',263);
  h.write(h.reduce((sum,b)=>sum+b,0).toString(8).padStart(6,'0')+'\0 ',148);
  return Buffer.concat([h,data,Buffer.alloc((512-data.length%512)%512)]);
}
let result;
try{
  let value;
  if(input.operation==='archive'){
    const bytes=gzipSync(Buffer.concat([...(input.entries??[]).map(row=>entry(row.path,Buffer.from(row.bodyBase64??'','base64'),row.mode)),Buffer.alloc(1024)]));
    writeFileSync(input.path,bytes);value={sha512:'sha512-'+createHash('sha512').update(bytes).digest('base64'),bytes:bytes.length};
  }else if(input.operation==='plan'){
    // The original early return has no work; project that as an empty plan.
    value=(await helpers.dependencyPlan(input.lock,new Set(input.selected),input.artifactRoot))??[];
  }
  else if(input.operation==='policy')value=await helpers.responsePolicy(input.dependency);
  else if(input.operation==='listing')value=helpers.listingPolicy({stdout:input.stdout,truncated:input.truncated});
  else if(input.operation==='unlink'){await rm(input.path,{force:true});value=null;}
  else if(input.operation==='install'){
    await helpers.materializePortableDependencies({packages:{[input.dependency.location]:{...input.dependency,os:['linux']}}},new Set([input.dependency.location]),input.originalRoot,input.temporaryRoot,input.executables);value=null;
  }else throw Error('Unknown portable fixture operation');
  result={ok:true,value:value??null};
}catch(error){result={ok:false,error:IvyError.from(error).toWire()};}
process.stdout.write(JSON.stringify({operation:result,...(['install','unlink'].includes(input.operation)?{actualTree:tree(input.actualRoot),originalTree:tree(input.originalRoot)}:{})}));
