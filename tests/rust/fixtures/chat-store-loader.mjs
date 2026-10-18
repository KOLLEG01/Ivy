import {existsSync,readFileSync} from 'node:fs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
const contracts=new URL('../../../packages/contracts/src/',import.meta.url);
const sdk=new URL('../../../packages/sdk/src/',import.meta.url);
const data=source=>'data:text/javascript;base64,'+Buffer.from(source).toString('base64');
const exports=data([
 `export {canonical,hashJson} from '${new URL('canonical.ts',contracts)}';`,
 `export {IvyError,requireThat} from '${new URL('errors.ts',contracts)}';`,
 `export {parseOperationId} from '${new URL('operation-id.ts',contracts)}';`,
 `export {hasUnresolvedOutcome} from '${new URL('receipt-archive.ts',sdk)}';`,
 `export {bundledSchema} from '${new URL('bundle.ts',contracts)}';`,
 `export {validateChat} from '${new URL('chat-validation.ts',contracts)}';`,
].join('\n'));
registerHooks({resolve(s,c,n){
 if(s==='../../../packages/sdk/src/node.js')return {url:exports,shortCircuit:true};
 if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}
 return n(s,c);
},load(u,c,n){
 if(u.endsWith('.ts'))return {format:'module',source:stripTypeScriptTypes(readFileSync(fileURLToPath(u),'utf8'),{mode:'transform'}),shortCircuit:true};
 return n(u,c);
}});
