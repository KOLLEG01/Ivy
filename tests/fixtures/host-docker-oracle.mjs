import {readFileSync,existsSync} from 'node:fs';
import {resolve,join} from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {createRequire,registerHooks} from 'node:module';
const input=JSON.parse(readFileSync(0,'utf8')),root=resolve(input.fixtureRoot),state={root,model:input.model??{},trace:[],directories:[],commandIndex:0,counts:{}};
globalThis.__ivyDockerFixture=state;
const require=createRequire(join(process.env.IVY_TEST_ORIGINAL_DEPENDENCIES,'package.json'));
Object.defineProperty(process,'platform',{value:state.model.platform??'linux'});
registerHooks({resolve(specifier,context,next){
 const boundary=/\/packages\/host-runtime\/src\/docker\.ts$/.test(context.parentURL??'');
 if(boundary&&['node:fs/promises','node:timers/promises','./config.js','./process.js','./linux-resources.js'].includes(specifier))return next(new URL('./host-docker-io.mjs',import.meta.url).href,context);
 if(boundary&&specifier==='node:path'&&state.model.pathStyle!=='native')return next('node:path/posix',context);
 if(specifier.startsWith('.')&&specifier.endsWith('.js')){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
 try{return next(specifier,context);}catch(error){if(/^ajv(?:\/|$)/.test(specifier))return next(pathToFileURL(require.resolve(specifier)).href,context);throw error;}
}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');state.IvyError=IvyError;
const module=await import('../../packages/host-runtime/src/docker.ts');
const io=await import('./host-docker-io.mjs');
const command=state.model.customCommand===false?undefined:async (_host,args,timeoutMs=10000,extraSecrets=[])=>{state.trace.push({op:'command',args,timeoutMs,extraSecrets});return io.next();};
const adapter=module.createDockerAdapter(command);
let operation;
try{
 let value;
 switch(input.operation){
 case 'containerName':value=module.containerName(...input.args);break;
 case 'dockerRestartPolicy':value=module.dockerRestartPolicy(...input.args);break;
 case 'inspectContainer':case 'stopContainer':case 'verifyContainerImage':value=await adapter[input.operation](...input.args);break;
 case 'startContainer':{
  let entered;const asleep=new Promise(resolve=>{entered=resolve;});state.sleepEntered=entered;
  const owner=await adapter.startContainer(...input.args);
  if(state.model.pauseObserver){await asleep;
   if(state.model.verifyOwner){try{await owner.verify();value={verified:true};}catch(error){value={verified:false,error:IvyError.from(error).toWire()};}}
   owner.dispose();const completed=await owner.completion;value={verification:value??null,completion:completed};
  }else value=await owner.completion;
  break;
 }
 default:throw Error('Unlisted original Docker operation');
 }
 operation={ok:true,value:value??null};
}catch(error){operation={ok:false,error:IvyError.from(error).toWire()};}
process.stdout.write(JSON.stringify({operation,trace:state.trace,directories:state.directories}));
