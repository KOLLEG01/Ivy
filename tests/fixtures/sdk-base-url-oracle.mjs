import {registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {inputs} from './whatwg-url-oracle.mjs';
import {IvyError} from '../../packages/contracts/src/errors.ts';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){
    const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(source)))return next(source.href,context);
  }
  return next(specifier,context);
}});
const {baseUrl}=await import('../../packages/sdk/src/client.ts');
const values=new Set([...inputs,'not-a-url','null','undefined','0','\ud800',
  'http://localhost/a//','https://example.invalid/a?','https://example.invalid/a#']);
const rows=[...values].map(input=>{
  let result;
  try{result={ok:true,value:baseUrl(input).href};}
  catch(error){result={ok:false,error:IvyError.from(error).toWire()};}
  return{input:JSON.stringify(input),result:JSON.stringify(result)};
});
process.stdout.write(JSON.stringify(rows));
