import {existsSync,readFileSync} from 'node:fs';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const input=JSON.parse(readFileSync(0,'utf8'));
try{
 let value;
 if(input.kind==='runtimeDependencies'){
  const dependencies=await import('../../packages/host-runtime/src/runtime-dependencies.ts');value=[...dependencies[input.operation](...input.args)];
 }else if(input.kind==='preparationTools'){
  const {preparationTools}=await import('../../packages/host-runtime/src/preparation-tools.ts');value=await preparationTools(...input.args);
 }else throw Error('Unknown build-input operation');
 process.stdout.write(JSON.stringify({ok:true,value}));
}catch(error){process.stdout.write(JSON.stringify({ok:false,error:IvyError.from(error).toWire()}));}
