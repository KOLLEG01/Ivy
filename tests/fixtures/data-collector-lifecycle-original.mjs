// The complete original entrypoint, engine, store and worker remain the oracle.
import {createRequire, registerHooks} from 'node:module';
import {existsSync, readFileSync} from 'node:fs';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {setTimeout as delay} from 'node:timers/promises';
import {join} from 'node:path';
const require=createRequire(process.env.IVY_TEST_PACKAGE_JSON||import.meta.url);
const dependencies=new Map(['ajv','ajv/dist/2020.js','ws','re2js','zod'].map(s=>[s,pathToFileURL(require.resolve(s)).href]));
dependencies.set('ws',new URL('./wrapper.mjs',dependencies.get('ws')).href);
// Avoid loading unrelated host-operation exports; the original main uses only these three.
const configURL=new URL('../../packages/host-runtime/src/config.ts',import.meta.url).href;
const healthURL=new URL('../../packages/host-runtime/src/health.ts',import.meta.url).href;
const host='data:text/javascript;base64,'+Buffer.from(`export {configurationPath,instanceConfig} from ${JSON.stringify(configURL)};export {HealthFile} from ${JSON.stringify(healthURL)};`).toString('base64');
registerHooks({resolve(s,c,n){
  if(dependencies.has(s))return n(dependencies.get(s),c);
  if(s.endsWith('packages/sdk/src/host.js'))return n(host,c);
  if(s.startsWith('.')&&s.endsWith('.js')){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}
  return n(s,c);
}});
if(process.argv[2]==='fingerprint') {
  const {TaskRunner}=await import('../../services/data-collector/src/runner.ts');
  const {IvyError}=await import('../../packages/contracts/src/errors.ts');
  const runner=new TaskRunner(process.argv[3],process.argv[3],{});
  const names=['A','a','z','Å','é','nested','token'];
  let value;
  try {value={hash:runner.authenticationFingerprint('fixture')};}catch(e){value={error:IvyError.from(e).code};}
  console.log(JSON.stringify({...value,names,order:names.map(a=>names.map(b=>Math.sign(a.localeCompare(b))))}));
  process.exit(0);
}
const {startDataCollector}=await import('../../services/data-collector/src/main.ts');
const path=process.argv[2], config=JSON.parse(readFileSync(path,'utf8'));
const runtime=await startDataCollector(path);
console.log(JSON.stringify({started:true}));
for(;;){
  await delay(50);
  try {if(JSON.parse(readFileSync(join(config.dataRoot,'health.json'),'utf8')).details==='DataCollector stopped.')break;} catch {}
}
await runtime.close();
