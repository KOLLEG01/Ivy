import {readFileSync,existsSync,lstatSync} from 'node:fs';
import {resolve,relative,isAbsolute,join} from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {createRequire,registerHooks} from 'node:module';
const input=JSON.parse(readFileSync(0,'utf8'));
const root=resolve(input.fixtureRoot),home=resolve(root,'account');
const local=relative(root,home);if(!local||isAbsolute(local)||local.startsWith('..'))throw Error('Invalid fixture home');
process.env.HOME=home;process.env.USERPROFILE=home;
const state={model:structuredClone(input.model??{}),files:structuredClone(input.model?.files??{}),trace:[],counts:{},commandIndex:0};
globalThis.__ivyLinuxFixture=state;
if(input.model){Object.defineProperty(process,'platform',{value:input.model.platform??'linux'});process.getuid=()=>input.model.uid??0;}
const require=createRequire(join(process.env.IVY_TEST_ORIGINAL_DEPENDENCIES,'package.json'));
registerHooks({resolve(specifier,context,next){
 const boundary=input.model&&/\/(?:linux-resources|linux-service-identity)\.ts$/.test(context.parentURL??'');
 if(boundary&&['node:fs/promises','node:child_process'].includes(specifier))return next(new URL('./host-linux-platform-io.mjs',import.meta.url).href,context);
 if(boundary&&specifier==='node:path')return next('node:path/posix',context);
 if(specifier.startsWith('.')&&specifier.endsWith('.js')){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
 try{return next(specifier,context);}catch(error){if(/^ajv(?:\/|$)/.test(specifier))return next(pathToFileURL(require.resolve(specifier)).href,context);throw error;}
}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const modules={resources:'../../packages/host-runtime/src/linux-resources.ts',identity:'../../packages/host-runtime/src/linux-service-identity.ts',bootstrap:'../../packages/host-runtime/src/bootstrap.ts',systemd:'../../packages/host-runtime/src/systemd.ts'};
const allowed={resources:['installationIdentity','linuxResourceScope','validateLinuxResourceScope','linuxSlice','assertLinuxResourceMembership','ensureLinuxSlice','inspectLinuxSlice','verifyLinuxResourceMembership'],identity:['linuxProcessUser','ensureLinuxProcessIdentity'],bootstrap:['configurationBootstrapPlan','linuxUnit','replaceableSystemdOwner'],systemd:['parseSystemdState','systemdUnitName','renderSystemdUnit']};
let operation;
try{
 if(input.module==='nativeMetadata'){
  const path=resolve(input.args[0]),local=relative(root,path);
  if(!local||isAbsolute(local)||local.startsWith('..'))throw Error('Metadata input leaves fixture');
  const value=lstatSync(path);
  operation={ok:true,value:{file:value.isFile(),symlink:value.isSymbolicLink(),uid:value.uid,gid:value.gid,mode:value.mode,size:value.size}};
 }else{
 if(!allowed[input.module]?.includes(input.operation))throw Error('Unlisted original fixture function');
 if(['ensureLinuxSlice','inspectLinuxSlice','verifyLinuxResourceMembership','ensureLinuxProcessIdentity'].includes(input.operation)&&!input.model)throw Error('Native administration is excluded from this fixture');
 const module=await import(modules[input.module]);let value=await module[input.operation](...input.args);if(value===undefined)value=null;
 operation={ok:true,value};
 }
}catch(error){operation={ok:false,error:IvyError.from(error).toWire()};}
process.stdout.write(JSON.stringify({operation,trace:state.trace,files:state.files}));
