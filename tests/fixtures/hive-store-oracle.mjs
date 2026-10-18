import {registerHooks,stripTypeScriptTypes} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
},load(url,context,next){
  if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
  return next(url,context);
}});
const {HiveStore}=await import('../../services/hive/src/store.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const input=JSON.parse(readFileSync(0,'utf8'));
let store;
const snapshot=()=>({principals:store.all('SELECT * FROM principals ORDER BY id'),credentials:store.all('SELECT * FROM credentials ORDER BY digest'),nodes:store.all('SELECT * FROM service_nodes ORDER BY id'),sessions:store.all('SELECT * FROM browser_sessions ORDER BY digest'),subscriptions:store.all('SELECT * FROM subscriptions ORDER BY node_id,name'),durable:store.all('SELECT * FROM durable_subscriptions ORDER BY node_id,name'),changes:[...store.changes]});
const rows=[];
try {
 store=new HiveStore(input.path);
 for(const step of input.steps){
  let value=null,error=null;
  try {
   switch(step.op){
    case 'configure':store.configureCredentials(step.credentials);break;
    case 'authenticate':value=store.authenticate(step.context);break;
    case 'sql':store.db.exec(step.sql);break;
    case 'invalidate':store.invalidate(step.scope);break;
    case 'transaction':store.transaction(()=>{store.db.exec(step.sql);store.invalidate(step.scope);if(step.fail)throw new IvyError('fixture_failure','Rollback requested.');});break;
    case 'reopen':store.close();store=new HiveStore(input.path);break;
    default:throw Error('Unknown trace action');
   }
  }catch(e){error=IvyError.from(e).toWire();}
  rows.push({value,error,state:snapshot()});
 }
 const definitions=store.all("SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name");
 const metadata=store.all("SELECT * FROM metadata WHERE key NOT IN ('cursor_secret','runtime_epoch') ORDER BY key");
 const topics=store.all('SELECT * FROM topics ORDER BY topic,version');
 process.stdout.write(JSON.stringify({rows,definitions,metadata,topics}));
}finally{store?.close();}
