import {existsSync,readFileSync,mkdirSync,writeFileSync,symlinkSync} from 'node:fs';
import {registerHooks} from 'node:module';
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
const maintenance=await import('../../packages/host-runtime/src/runtime-maintenance.ts');
const capture=async work=>{try{return {ok:true,value:await work()};}catch(error){return {ok:false,error:IvyError.from(error).toWire()};}};
const input=JSON.parse(readFileSync(0,'utf8'));
function setup(actions){
  for(const a of actions??[]){
    mkdirSync(path.dirname(a.path),{recursive:true});
    if(a.kind==='directory')mkdirSync(a.path,{recursive:true});
    else if(a.kind==='file')writeFileSync(a.path,Object.hasOwn(a,'json')?JSON.stringify(a.json):a.content??'fixture');
    else if(a.kind==='link')symlinkSync(a.target,a.path,process.platform==='win32'?'junction':'dir');
    else throw Error('Unknown setup action');
  }
}
let output;
if(input.kind==='maintenance'){
  output=[];
  for(const c of input.cases){
    setup(c.setup);
    output.push({id:c.id,marker:await capture(()=>maintenance.runtimeResetMarker(c.config)),active:await capture(()=>maintenance.runtimeResetActive(c.config)),hive:await capture(()=>maintenance.hiveResetBootstrapMarker(c.dataRoot))});
  }
}else throw Error('Unknown oracle request');
process.stdout.write(JSON.stringify(output));
