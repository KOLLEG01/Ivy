import {existsSync,readFileSync,readdirSync,lstatSync} from 'node:fs';
import {join,relative,isAbsolute,resolve} from 'node:path';
import {registerHooks} from 'node:module';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {createRequire} from 'node:module';
import {createHash} from 'node:crypto';
const dependencyRoot=process.env.IVY_TEST_ORIGINAL_DEPENDENCIES;
const dependencyRequire=dependencyRoot?createRequire(join(dependencyRoot,'package.json')):null;
registerHooks({resolve(specifier,context,next){
  if(specifier==='typescript'&&process.env.IVY_TEST_TYPESCRIPT_MODULE)return next(pathToFileURL(process.env.IVY_TEST_TYPESCRIPT_MODULE).href,context);
  if(specifier.startsWith('.')&&specifier.endsWith('.js')){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  try{return next(specifier,context);}catch(error){if(dependencyRequire&&(/^ajv(?:\/|$)/.test(specifier)))return next(pathToFileURL(dependencyRequire.resolve(specifier)).href,context);throw error;}
}});
const input=JSON.parse(readFileSync(0,'utf8'));
// All original mutation roots must be explicit descendants of this one fixture.
// This fixture never invokes account-home resolution or reset operations.
const boundary=resolve(input.fixtureRoot);
function contained(path){const local=relative(boundary,resolve(path));if(!local||isAbsolute(local)||local==='..'||local.startsWith('../')||local.startsWith('..\\'))throw Error('Original preparation root leaves its explicit fixture');}
for(const path of [input.root,input.bootstrap,...['runtimeRoot','artifactRoot','stagingRoot'].map(k=>input.host[k]),input.snapshot.sourceRoot,input.snapshot.manifestPath])contained(path);
const {prepareCandidate}=await import('../../packages/host-runtime/src/prepare.ts');
const {HostJournal}=await import('../../packages/host-runtime/src/journal.ts');
const {preparationIdForBuild}=await import('../../packages/host-runtime/src/preparations.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const files=[];
function tree(directory,base){for(const name of readdirSync(directory)){const file=join(directory,name),m=lstatSync(file);if(m.isDirectory())tree(file,base);else if(m.isFile()){const bytes=readFileSync(file);files.push({path:relative(base,file).replaceAll('\\','/'),bytes:bytes.length,hash:'sha256:'+createHash('sha256').update(bytes).digest('hex')});}else throw Error('Unexpected published entry');}}
let operation,candidate=null,record=null,manifest=null,saved=null,reused=null,archiveValid=null;
try{candidate=await prepareCandidate(input.snapshot,'fixture',input.host,input.bootstrap);operation={ok:true,value:candidate};if(input.reuse)reused=await prepareCandidate(input.snapshot,'fixture',input.host,input.bootstrap);}
catch(error){operation={ok:false,error:IvyError.from(error).toWire()};}
if(candidate){
  const journal=new HostJournal(input.host);try{saved=journal.candidate(candidate.candidateId);manifest=journal.manifest(candidate.candidateId);}finally{journal.close();}
  const directory=join(input.host.stagingRoot,'build-'+preparationIdForBuild(candidate.buildId));record=JSON.parse(readFileSync(join(directory,'preparation.json'),'utf8'));
  tree(candidate.artifactRoot,candidate.artifactRoot);files.sort((a,b)=>a.path<b.path?-1:a.path>b.path?1:0);
  const bytes=readFileSync(candidate.archivePath);archiveValid=bytes.length>0&&candidate.archiveHash==='sha256:'+createHash('sha256').update(bytes).digest('hex');
}else{
  const names=existsSync(input.host.stagingRoot)?readdirSync(input.host.stagingRoot).filter(name=>name.startsWith('build-')&&name.length===42):[];
  if(names.length>1)throw Error('Fixture has multiple preparation attempts');if(names.length)record=JSON.parse(readFileSync(join(input.host.stagingRoot,names[0],'preparation.json'),'utf8'));
}
process.stdout.write(JSON.stringify({operation,candidate,record,manifest,saved,reused,archiveValid,files}));
