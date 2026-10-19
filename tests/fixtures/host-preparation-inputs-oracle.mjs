import {existsSync,readFileSync} from 'node:fs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}return next(specifier,context);}});
const input=JSON.parse(readFileSync(0,'utf8'));
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const prepare=readFileSync(new URL('../../packages/host-runtime/src/prepare.ts',import.meta.url),'utf8'),artifact=readFileSync(new URL('../../packages/host-runtime/src/artifact.ts',import.meta.url),'utf8'),config=readFileSync(new URL('../../packages/host-runtime/src/config.ts',import.meta.url),'utf8');
function span(source,start,end){const a=source.indexOf(start),b=source.indexOf(end,a+start.length);if(a<0||b<a)throw Error('Original input-phase boundary changed');return source.slice(a,b);}
const source=`
import {createHash} from 'node:crypto';
import {lstat,stat,open,readdir,readFile} from 'node:fs/promises';
import {join,relative,resolve,isAbsolute} from 'node:path';
import {mapConcurrent} from '${new URL('../../packages/host-runtime/src/concurrency.ts',import.meta.url)}';
import {requireThat} from '${new URL('../../packages/contracts/src/errors.ts',import.meta.url)}';
import {hashJson,digest} from '${new URL('../../packages/contracts/src/canonical.ts',import.meta.url)}';
import {candidateBuildId} from '${new URL('../../packages/host-runtime/src/candidate-identity.ts',import.meta.url)}';
const process=${JSON.stringify(input.profile??{platform:process.platform,arch:process.arch,version:process.version})};
${span(config,'export async function jsonFile<','\nexport async function atomicJson(')}
${span(config,'export function inside(', '\nexport async function instanceConfig(')}
${span(artifact,'async function hashFile(', '\nexport function releaseManifest(')}
${span(artifact,'export async function artifactFiles(', '\nexport async function verifyCandidate(')}
${span(prepare,'const dependencyCacheKey =', '\nconst workspaceLinkEntries =')}
export function component(componentId){${span(prepare,"  requireThat(/^[a-z][a-z0-9-]*$/.test(componentId)",'\n  // The source snapshot')}}
export function planGate(componentId,plan){${span(prepare,"  if (plan.kind === 'app') requireThat",'\n  const packageJson =')}}
export async function manager(root){const snapshot={sourceRoot:root};${span(prepare,'  const packageJson =','\n  // Caller log wrappers')}return packageJson.packageManager;}
export async function identity(root,componentId,version,packageManager){const snapshot={sourceRoot:root},plan={version},packageJson={packageManager};
${span(prepare,'  const dependencyLockHash =', '\n    const dependencyCacheRoot =')}
${span(prepare,'  const sourceIdentity =', '\n  const progress =')}
return {dependencyLockHash,dependencyKey,sourceIdentity,buildId};}
`;
const original=await import('data:text/javascript,'+encodeURIComponent(stripTypeScriptTypes(source,{mode:'transform'})));
let operation,nativeError;
try{
  const value=input.operation==='component'?original.component(input.componentId)
    :input.operation==='plan'?original.planGate(input.componentId,input.plan)
    :input.operation==='manager'?await original.manager(input.root)
    :input.operation==='identity'?await original.identity(input.root,input.componentId,input.version,input.packageManager)
    :{paths:input.paths,matrix:input.paths.map(a=>input.paths.map(b=>Math.sign(a.localeCompare(b)))),profile:{locale:Intl.DateTimeFormat().resolvedOptions().locale,icu:process.versions.icu,node:process.version}};
  operation={ok:true,value:value??null};
}catch(error){nativeError={name:error.name,...(error.code?{code:error.code}:{})};operation={ok:false,error:IvyError.from(error).toWire()};}
process.stdout.write(JSON.stringify({operation,...(nativeError?{nativeError}:{})}));
