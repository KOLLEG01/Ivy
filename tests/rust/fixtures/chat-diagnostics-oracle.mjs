import {readFileSync,existsSync} from 'node:fs';
import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){
    const url=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);
    if(existsSync(fileURLToPath(url)))return next(url.href,context);
  }
  return next(specifier,context);
},load(url,context,next){
  if(url.endsWith('.ts'))return {format:'module',source:stripTypeScriptTypes(readFileSync(fileURLToPath(url),'utf8'),{mode:'transform'}),shortCircuit:true};
  return next(url,context);
}});
const {hashJson}=await import('../../../packages/contracts/src/canonical.ts');
const source=readFileSync('services/chat-bridge/src/diagnostic-notifications.ts','utf8');
const helpers=source.slice(source.indexOf('const noticeKey'),source.indexOf('/** Shared operational notices'));
const run=new Function('hashJson','input',stripTypeScriptTypes(helpers,{mode:'transform'})+'; return input.map(v => ({state:groupedNoticeState(v.saved), notices:v.diagnostics.map(diagnosticNotice)}));');
process.stdout.write(JSON.stringify(run(hashJson,JSON.parse(readFileSync(0,'utf8')))));
