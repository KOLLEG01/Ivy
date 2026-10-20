import {readFileSync,writeFileSync,existsSync} from 'node:fs';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
import path from 'node:path';
const [rootInput,nonce,mode,ownerRoot,controlPath]=process.argv.slice(2),root=path.resolve(rootInput);
if(!path.basename(path.dirname(root)).startsWith('ivy-config-'))throw Error('Disposable process root required');
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')){const url=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(url)))return next(url.href,context);}return next(specifier,context);}});
let lock;
if(ownerRoot){const {ExecutorLock}=await import('../../packages/host-runtime/src/journal.ts');lock=new ExecutorLock(ownerRoot);}
writeFileSync(path.join(root,'identity.json'),JSON.stringify({pid:process.pid,nonce,mode}));
if(mode==='exit259')setTimeout(()=>{lock?.close();process.exit(259);},350);
if(mode==='cooperative')setTimeout(()=>{lock?.close();process.exit(86);},15_000).unref();
const timer=setInterval(()=>{
 if(mode!=='cooperative')return;
 try{const c=JSON.parse(readFileSync(controlPath,'utf8'));if(c.instanceId==='fixture'&&c.bootId==='fixture-boot'&&c.launchId==='fixture-launch'&&c.action==='shutdown'){lock?.close();clearInterval(timer);process.exit(0);}}catch{}
},25);
