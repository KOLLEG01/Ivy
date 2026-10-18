import {existsSync,readFileSync,mkdirSync,writeFileSync,symlinkSync,lstatSync,readdirSync} from 'node:fs';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
import {homedir} from 'node:os';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const host=await import('../../packages/host-runtime/src/host-config.ts');
const layout=await import('../../packages/host-runtime/src/layout.ts');
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
function tree(root){
  const values=[];
  function walk(directory){
    for(const name of readdirSync(directory).sort()){
      const full=path.join(directory,name),s=lstatSync(full);
      values.push({path:path.relative(root,full).split(path.sep).join('/'),kind:s.isSymbolicLink()?'link':s.isDirectory()?'directory':'file'});
      if(s.isDirectory()&&!s.isSymbolicLink())walk(full);
    }
  }
  if(existsSync(root))walk(root);
  return values;
}
let output;
if(input.kind==='account'){
  output={osHome:await capture(()=>homedir()),accountHome:await capture(()=>layout.accountHome()),defaultPath:await capture(()=>layout.defaultHostConfigPath()),selected:process.env.IVY_ACCOUNT_UTF16??null};
}else if(input.kind==='paths'){
  output=[];
  for(const c of input.cases){
    setup(c.setup);const result=await capture(()=>c.operation==='realpath'?import('node:fs/promises').then(fs=>fs.realpath(c.path)):layout.resolvedFuturePath(c.path));
    output.push({id:c.id,result,tree:tree(c.root)});
  }
}else if(input.kind==='maintenance'){
  output=[];
  for(const c of input.cases){
    setup(c.setup);
    output.push({id:c.id,marker:await capture(()=>maintenance.runtimeResetMarker(c.config)),active:await capture(()=>maintenance.runtimeResetActive(c.config)),hive:await capture(()=>maintenance.hiveResetBootstrapMarker(c.dataRoot))});
  }
}else if(input.kind==='configuration'){
  output=[];
  for(const c of input.cases){
    setup(c.setup);
    const result=await capture(()=>c.operation==='file'?host.hostConfig(c.configurationPath):host.checkedHostConfig(c.config,c.configurationPath));
    output.push({id:c.id,result,tree:tree(c.root)});
  }
}else throw Error('Unknown oracle request');
process.stdout.write(JSON.stringify(output));
