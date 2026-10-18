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
const {loadServiceConfiguration}=await import('../../services/secretary/src/service-config.ts');
const input=JSON.parse(readFileSync(0,'utf8'));
const invalid=()=>new IvyError('invalid_arguments','Protected fixture input is invalid.','not_executed',{marker:'caller-owned'});
try{process.stdout.write(JSON.stringify({ok:true,value:await loadServiceConfiguration(input.path,input.maximum,invalid)}));}
catch(error){process.stdout.write(JSON.stringify({ok:false,error:IvyError.from(error).toWire()}));}
