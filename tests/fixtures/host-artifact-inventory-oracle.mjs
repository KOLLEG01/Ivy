import {existsSync,readFileSync} from 'node:fs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
}});
const input=JSON.parse(readFileSync(0,'utf8'));
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const artifact=readFileSync(new URL('../../packages/host-runtime/src/artifact.ts',import.meta.url),'utf8');
const config=readFileSync(new URL('../../packages/host-runtime/src/config.ts',import.meta.url),'utf8');
const prepare=readFileSync(new URL('../../packages/host-runtime/src/prepare.ts',import.meta.url),'utf8');
function span(source,start,end){const a=source.indexOf(start),b=source.indexOf(end,a+start.length);if(a<0||b<a)throw Error('Original inventory boundary changed');return source.slice(a,b);}
const source=`
import {createHash} from 'node:crypto';
import {lstat,open,readdir} from 'node:fs/promises';
import {join,relative,resolve,isAbsolute} from 'node:path';
import {mapConcurrent} from '${new URL('../../packages/host-runtime/src/concurrency.ts',import.meta.url)}';
import {requireThat} from '${new URL('../../packages/contracts/src/errors.ts',import.meta.url)}';
import {hashJson} from '${new URL('../../packages/contracts/src/canonical.ts',import.meta.url)}';
import {candidateBuildId} from '${new URL('../../packages/host-runtime/src/candidate-identity.ts',import.meta.url)}';
${span(config,'export function inside(', '\nexport async function instanceConfig(')}
${span(artifact,'async function hashFile(', '\nexport function releaseManifest(')}
${span(artifact,'export async function artifactFiles(', '\nexport async function verifyCandidate(')}
export async function identity(root,componentId,version){const snapshot={sourceRoot:root},plan={version};
${span(prepare,'  const sourceIdentity =', '\n  const progress =')}
return {sourceIdentity,buildId};}
`;
const original=await import('data:text/javascript,'+encodeURIComponent(stripTypeScriptTypes(source,{mode:'transform'})));
let operation,nativeError;
try{
  const value=input.operation==='order'?{paths:input.paths,matrix:input.paths.map(a=>input.paths.map(b=>Math.sign(a.localeCompare(b)))),profile:{locale:Intl.DateTimeFormat().resolvedOptions().locale,icu:process.versions.icu,node:process.version,platform:process.platform,arch:process.arch}}
    :input.operation==='identity'?await original.identity(input.root,input.componentId,input.version)
    :await original.artifactFiles(input.root);
  operation={ok:true,value};
}catch(error){nativeError={name:error.name,...(error.code?{code:error.code}:{})};operation={ok:false,error:IvyError.from(error).toWire()};}
process.stdout.write(JSON.stringify({operation,...(nativeError?{nativeError}:{})}));
