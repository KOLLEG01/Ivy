import {readFileSync} from 'node:fs';
import {readFile,readdir} from 'node:fs/promises';
import {join} from 'node:path';
import {stripTypeScriptTypes,registerHooks} from 'node:module';
import {existsSync} from 'node:fs';
import {fileURLToPath,pathToFileURL} from 'node:url';
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}return next(specifier,context);}});
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const typescript=(await import(process.env.IVY_TEST_TYPESCRIPT_MODULE?pathToFileURL(process.env.IVY_TEST_TYPESCRIPT_MODULE).href:'typescript')).default;
if(typescript.version!=='5.9.3')throw Error('Require the accepted original TypeScript 5.9.3 parser');
const input=JSON.parse(readFileSync(0,'utf8'));
const source=readFileSync(new URL('../../packages/host-runtime/src/prepare.ts',import.meta.url),'utf8');
const start=source.indexOf('async function emittedRuntimePackages('),end=source.indexOf('\nconst selectedDependencyPath',start);
if(start<0||end<start)throw Error('Original emitted inventory boundary changed');
const original=source.slice(start,end),visitorStart=original.indexOf('      const visit ='),visitorEnd=original.indexOf('      visit(parsed);',visitorStart)+'      visit(parsed);'.length;
if(visitorStart<0||visitorEnd<visitorStart)throw Error('Original AST visitor boundary changed');
const visit=new Function('typescript','parsed','add',stripTypeScriptTypes(original.slice(visitorStart,visitorEnd),{mode:'transform'}));
const parsed=[];
const recording={...typescript,createSourceFile(...args){
  const tree=typescript.createSourceFile(...args),imports=[];visit(typescript,tree,value=>imports.push(value));parsed.push({path:args[0],source:args[1],imports});return tree;
}};
const emitted=new Function('typescript','readdir','join','readFile',stripTypeScriptTypes(original,{mode:'transform'})+'\nreturn emittedRuntimePackages;')(recording,readdir,join,readFile);
let operation;
try{operation={ok:true,value:[...await emitted(input.root)]};}
catch(error){operation={ok:false,error:IvyError.from(error).toWire()};}
process.stdout.write(JSON.stringify({operation,parsed}));
