import {readFileSync,existsSync,readdirSync,lstatSync} from 'node:fs';
import {join,relative,isAbsolute,resolve} from 'node:path';
import {createRequire,registerHooks} from 'node:module';
import {fileURLToPath,pathToFileURL} from 'node:url';
const input=JSON.parse(readFileSync(0,'utf8'));
const boundary=resolve(input.fixtureRoot);
function contained(path){const local=relative(boundary,resolve(path));if(!local||isAbsolute(local)||local==='..'||local.startsWith('../')||local.startsWith('..\\'))throw Error('Original operation leaves its explicit fixture');}
contained(input.home);
// Child-local account defaults are fixed before any original module import.
// This fixture never invokes runtime-reset or account cleanup operations.
process.env.USERPROFILE=input.home;process.env.HOME=input.home;
const dependencies=process.env.IVY_TEST_ORIGINAL_DEPENDENCIES;
const require=dependencies?createRequire(join(dependencies,'package.json')):null;
registerHooks({resolve(specifier,context,next){
 if(specifier.startsWith('.')&&specifier.endsWith('.js')){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
 try{return next(specifier,context);}catch(error){if(require&&/^ajv(?:\/|$)/.test(specifier))return next(pathToFileURL(require.resolve(specifier)).href,context);throw error;}
}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
let operation,written=[],accepted=[],selected=null;
try{
 let value;
 if(input.operation==='settings'){
  contained(input.artifactRoot);
  const {candidateBoundSettings}=await import('../../packages/host-runtime/src/target.ts');
  value=await candidateBoundSettings(input.component,input.settings,input.artifactRoot);
 }else if(input.operation==='storage'){
  for(const key of ['runtimeRoot','artifactRoot','stagingRoot'])contained(input.host[key]);
  for(const instance of input.host.instances)if(instance.paths)for(const path of Object.values(instance.paths))contained(path);
  const {verifyServiceStorage}=await import('../../packages/host-runtime/src/service-storage.ts');
  verifyServiceStorage(input.host,input.instanceId,input.plan,input.witness);value=null;
 }else if(input.operation==='stopped'){
  const {stoppedTarget}=await import('../../packages/host-runtime/src/target.ts');
  value=stoppedTarget(input.target);
 }else if(input.operation==='materialize'){
  for(const key of ['runtimeRoot','artifactRoot','stagingRoot'])contained(input.host[key]);
  for(const instance of input.host.instances)if(instance.paths)for(const path of Object.values(instance.paths))contained(path);
  contained(input.candidate.artifactRoot);contained(input.candidate.manifestPath);
  contained(join(input.host.runtimeRoot,'instances',input.instanceId,'configurations'));
  const {HostJournal}=await import('../../packages/host-runtime/src/journal.ts');
  const {materializeTarget}=await import('../../packages/host-runtime/src/target.ts');
  const journal=new HostJournal(input.host);
  try{
   journal.saveCandidate(input.candidate,input.manifest);
   value=await materializeTarget(journal,input.instanceId,input.candidate.candidateId,input.enabled,input.basis);
  }finally{selected=journal.target(input.instanceId);journal.close();}
 }else throw Error('Unknown fixture operation');
 operation={ok:true,value};
}catch(error){operation={ok:false,error:IvyError.from(error).toWire()};}
if(input.operation==='materialize'){
 const directory=join(input.host.runtimeRoot,'instances',input.instanceId,'configurations');
 if(existsSync(directory))for(const revision of readdirSync(directory))written.push({revision,config:JSON.parse(readFileSync(join(directory,revision,'config.json'),'utf8'))});
 const snapshots=join(input.host.runtimeRoot,'accepted-configurations');
 if(existsSync(snapshots)&&lstatSync(snapshots).isDirectory())for(const file of readdirSync(snapshots))accepted.push({revision:file.slice(0,-'.instance.json'.length),config:JSON.parse(readFileSync(join(snapshots,file),'utf8'))});
}
process.stdout.write(JSON.stringify({operation,written,accepted,selected}));
