import {existsSync,readFileSync} from 'node:fs';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
const contracts=new URL('../../../packages/contracts/src/',import.meta.url);
const sdk='data:text/javascript;base64,'+Buffer.from([
  `export {bundledSchema} from '${new URL('bundle.ts',contracts)}';`,
  `export {validateChat} from '${new URL('chat-validation.ts',contracts)}';`,
  readFileSync(new URL('../../../packages/sdk/src/main-chat.ts',import.meta.url),'utf8').match(/^export const chatInterfaceVersion = .+$/m)[0],
].join('\n')).toString('base64');
registerHooks({resolve(specifier,context,next){
  if(specifier==='../../../packages/sdk/src/node.js')return {url:sdk,shortCircuit:true};
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const url=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(url)))return next(url.href,context);}
  return next(specifier,context);
}});
const {chatRegistry}=await import('../../../services/chat-bridge/src/registry.ts');
process.stdout.write(JSON.stringify(chatRegistry()));
