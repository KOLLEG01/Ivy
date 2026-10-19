import {existsSync, readFileSync} from 'node:fs';
import {stat} from 'node:fs/promises';
import {join} from 'node:path';
import {registerHooks, createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}return next(specifier,context);}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {canonical}=await import('../../packages/contracts/src/canonical.ts');
const {atomicJson,jsonFile}=await import('../../packages/host-runtime/src/config.ts');
const {retainedBootstrapPlans}=await import('../../packages/host-runtime/src/bootstrap.ts');
const {servicePaths,hostConfigurationPath}=await import('../../packages/host-runtime/src/layout.ts');
const {validateHost}=await import('../../packages/contracts/src/host-validation.ts');
const input=JSON.parse(readFileSync(0,'utf8'));
const ts=createRequire(import.meta.url)(process.env.IVY_TEST_TYPESCRIPT_MODULE??'typescript');
const source=readFileSync(new URL('../../packages/host-runtime/src/bootstrap-config-restart.ts',import.meta.url),'utf8');
const js=ts.transpileModule(source,{compilerOptions:{target:ts.ScriptTarget.ESNext,module:ts.ModuleKind.ESNext}}).outputText;
const parsed=ts.createSourceFile('bootstrap-config-restart.js',js,ts.ScriptTarget.ESNext,true,ts.ScriptKind.JS);
const functionText=name=>{
  const declaration=parsed.statements.find(node=>ts.isFunctionDeclaration(node)&&node.name?.text===name);
  if(!declaration)throw Error('Original restart function span changed');
  return declaration.getText(parsed).replace(/^export\s+/,'');
};
const events=[],clock={at:input.at??1760000000000},states=input.states??[{owners:[]}];let inspected=0;
class DateAt extends Date{constructor(...args){super(...(args.length?args:[clock.at]));}static now(){return clock.at;}}
const os={async inspect(plans){events.push({kind:'inspect',plans});const state=states[Math.min(inspected++,states.length-1)];clock.at+=state.advance??0;if(state.error)throw new IvyError(state.error,'Fixture owner inspection failed.');return state.owners;}};
const exec=async(_file,args)=>{events.push({kind:'command',action:args[0],name:args[2]});if(input.failAction===args[0])throw Error('Fixture scheduler failure');};
const restart=new Function('bootstrapOs','exec','scheduler','runtimeEnvironment','requireThat','IvyError','Date',`return (${functionText('restartWindowsOwner')});`)(()=>os,exec,'fixture-scheduler',()=>({}),requireThat,IvyError,DateAt);
const reconcile=new Function('stat','join','canonical','atomicJson','jsonFile','retainedBootstrapPlans','servicePaths','hostConfigurationPath','validateHost','requireThat','Date','globalThis','restartWindowsOwner',`return (${functionText('reconcileBootstrapConfigurationRestarts')});`)(stat,join,canonical,atomicJson,jsonFile,retainedBootstrapPlans,servicePaths,hostConfigurationPath,validateHost,requireThat,DateAt,{process:{platform:input.platform??process.platform,uptime:()=> (clock.at-input.startedAt)/1000}},restart);
const wrap=async work=>{try{return{ok:true,value:await work()};}catch(error){return{ok:false,error:IvyError.from(error).toWire()};}};
const output=await wrap(async()=>{
  if(input.kind==='plans')return await retainedBootstrapPlans(input.config);
  if(input.kind==='windows'){await restart(input.plans,input.name,input.distribution);return events;}
  const results=[];
  for(let step=0;step<(input.times??1);step++)results.push(await wrap(()=>reconcile(input.config,input.own??undefined,input.distribution??undefined)));
  const files=[];for(const path of input.configFiles??[])files.push(await jsonFile(path));
  let marker=null;if(input.markerPath){try{marker=await jsonFile(input.markerPath);}catch(error){if(error.code!=='ENOENT')throw error;}}
  return {results,files,marker,events};
});
process.stdout.write(JSON.stringify({output,events,sourceHash:createHash('sha256').update(source).digest('hex'),restartFunction:functionText('restartWindowsOwner'),reconcileFunction:functionText('reconcileBootstrapConfigurationRestarts')}));
