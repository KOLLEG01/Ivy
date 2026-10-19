import {registerHooks,stripTypeScriptTypes,syncBuiltinESMExports} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import crypto from 'node:crypto';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
},load(url,context,next){
  if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
  return next(url,context);
}});
const {HiveStore}=await import('../../services/hive/src/store.ts');
const {Objects}=await import('../../services/hive/src/objects.ts');
const {Events}=await import('../../services/hive/src/events.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const input=JSON.parse(readFileSync(0,'utf8'));
let store;
const tableHash=(table,columns)=>'sha256:'+createHash('sha256').update(store.get(`SELECT json_group_array(json_array(${columns})) AS encoded FROM (SELECT rowid AS record_id,* FROM ${table} ORDER BY rowid)`).encoded).digest('hex');
const snapshot=()=>({contracts:tableHash('contracts','key,version,definition'),families:store.all('SELECT * FROM contract_families ORDER BY key'),objects:store.all('SELECT * FROM objects ORDER BY id'),fts:store.all('SELECT rowid,object_id,name,text FROM object_fts ORDER BY rowid'),revisions:tableHash('revisions',"record_id,object_id,revision,contract_key,contract_version,encoding,CASE WHEN typeof(content)='blob' THEN hex(content) ELSE content END,content_hash,byte_length,created_at,references_json"),references:store.all('SELECT * FROM revision_references ORDER BY source_object_id,source_revision,name'),principals:store.all('SELECT * FROM principals ORDER BY id'),credentials:store.all('SELECT * FROM credentials ORDER BY digest'),nodes:store.all('SELECT * FROM service_nodes ORDER BY id'),sessions:store.all('SELECT * FROM browser_sessions ORDER BY digest'),subscriptions:store.all('SELECT * FROM subscriptions ORDER BY node_id,name'),durable:store.all('SELECT * FROM durable_subscriptions ORDER BY node_id,name'),changes:[...store.changes],mutations:tableHash('mutations','record_id,principal_id,mutation_id,request_hash,issued_at_ms,completed_at_ms,result_kind,result_json,receipt_bytes'),events:tableHash('events','sequence,topic,topic_version,source,occurred_at,mutation_id,payload'),metadata:store.all("SELECT * FROM metadata WHERE key NOT IN ('cursor_secret','runtime_epoch') ORDER BY key"),retention:store.all('SELECT * FROM retention_families ORDER BY contract_key')});
const rows=[];
try {
 store=new HiveStore(input.path);
 for(const original of input.steps){
  const step=original.bindings?structuredClone(original):original;
  const set=(value,pointer,next)=>{const fields=pointer.split('/').slice(1).map(s=>s.replaceAll('~1','/').replaceAll('~0','~'));for(const field of fields.slice(0,-1))value=value[field];value[fields.at(-1)]=next;};
  for(const binding of step.bindings??[])set(step,binding.at,rows[binding.from].value.object.id);
  let value=null,error=null;
  try {
   switch(step.op){
    case 'register':{
      const definition={...step.definition};for(const [field,units]of Object.entries(step.definitionUnits??{}))definition[field]=String.fromCharCode(...units);
      value=new Objects(store).register(definition,step.owner);if(step.lossless)value={encoded:JSON.stringify(value)};break;
    }
    case 'registerAgent':{
      const realNow=Date.now;Date.now=()=>step.now;
      try{value=new Objects(store).registerAgent(store.authenticate(step.context),step.params);}finally{Date.now=realNow;}
      break;
    }
    case 'getContract':value=new Objects(store).getContract(step.key,step.version);break;
    case 'decode':{
      const content={...step.content};if(step.units)content.value=String.fromCharCode(...step.units);
      const result=new Objects(store).decode(step.contract,content);
      const kind=typeof result.data==='string'?'text':'bytes';
      value={kind,data:result.byteLength<=1024?(kind==='text'?result.data:Buffer.from(result.data).toString('base64')):null,contentHash:result.contentHash,byteLength:result.byteLength};
      break;
    }
    case 'write':{
      const RealDate=Date;
      const realUUID=crypto.randomUUID;
      if(step.createdId){crypto.randomUUID=()=>step.createdId;syncBuiltinESMExports();}
      if(new RealDate(step.now).toISOString()!==step.nowText)throw Error('Fixture clock mismatch');
      globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[step.now]));}static now(){return step.now;}};
      try{
        const context=store.authenticate(step.context),objects=new Objects(store);
        const contentUnits=params=>{if(step.contentUnits)params.content.value=String.fromCharCode(...step.contentUnits);for(const [pointer,units]of Object.entries(step.parameterUnits??{}))set(params,pointer,String.fromCharCode(...units));return params;};
        const changes=step.watchChanges?Number(store.get('SELECT total_changes() AS n').n):0;
        value=store.withBudget(()=>{
          if(step.mode==='prepared')return objects.writeHivePrepared(context,step.method,step.params,()=>{
            store.db.exec(step.prepareSql??'');
            if(step.prepareFail)throw new IvyError('fixture_failure','Preparation requested failure.');
            return contentUnits(structuredClone(step.preparedParams));
          });
          const params=contentUnits(structuredClone(step.params));
          return step.mode==='hive'?objects.writeHive(context,step.method,params):objects.write(context,params,step.mode==='preserve');
        },step.overdue?-1:10000);
        if(step.watchChanges)value={result:value,changes:Number(store.get('SELECT total_changes() AS n').n)-changes};
      }finally{globalThis.Date=RealDate;crypto.randomUUID=realUUID;syncBuiltinESMExports();}
      break;
    }
    case 'events':{
      const RealDate=Date;
      if(new RealDate(step.now).toISOString()!==step.nowText)throw Error('Fixture clock mismatch');
      globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[step.now]));}static now(){return step.now;}};
      try{
        const events=new Events(store),params=structuredClone(step.params);
        for(const [pointer,units]of Object.entries(step.parameterUnits??{}))set(params,pointer,String.fromCharCode(...units));
        const context=step.context?{...store.authenticate(step.context),serviceNodeId:step.context.serviceNodeId,generation:1}:null;
        value=store.withBudget(()=>{
          switch(step.method){
            case 'registerTopic':events.registerTopic(params,step.owner);return null;
            case 'publish':return events.publish(context,params);
            case 'subscribe':return events.subscribe(context,params);
            case 'ack':return events.ack(context,params);
            case 'unsubscribe':return events.unsubscribe(context,params);
            case 'read':return events.read(params);
            case 'head':return events.head();
            default:throw Error('Unknown event method');
          }
        },step.overdue?-1:10000);
        if(step.lossless)value={encoded:JSON.stringify(value)};
      }finally{globalThis.Date=RealDate;}
      break;
    }
    case 'readObject':value=new Objects(store).read({objectId:step.objectId,revision:step.revision});break;
    case 'configure':store.configureCredentials(step.credentials);break;
    case 'authenticate':value=store.authenticate(step.context);break;
    case 'sql':store.db.exec(step.sql);break;
    case 'invalidate':store.invalidate(step.scope);break;
    case 'transaction':store.transaction(()=>{store.db.exec(step.sql);store.invalidate(step.scope);if(step.fail)throw new IvyError('fixture_failure','Rollback requested.');});break;
    case 'reopen':store.close();store=new HiveStore(input.path);break;
    case 'reorder':case 'archive':case 'delete':case 'move':{
      const RealDate=Date;
      if(new RealDate(step.now).toISOString()!==step.nowText)throw Error('Fixture clock mismatch');
      globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[step.now]));}static now(){return step.now;}};
      try{
        const context={...store.authenticate(step.context),serviceNodeId:step.context.serviceNodeId};
        const params=structuredClone(step.params);
        for(const [pointer,units]of Object.entries(step.parameterUnits??{}))set(params,pointer,String.fromCharCode(...units));
        value=store.withBudget(()=>new Objects(store)[step.op](context,params),step.overdue?-1:10000);
      }finally{globalThis.Date=RealDate;}
      break;
    }
    case 'mutate':{
      const realNow=Date.now;Date.now=()=>step.now;
      try{value=store.mutate(step.context,step.method,step.params,()=>{
        store.db.exec(step.sql??'');if(step.scope)store.invalidate(step.scope);
        if(step.fail)throw new IvyError('fixture_failure','Rollback requested.');
        return {value:step.value,kind:step.kind,reference:step.reference};
      });}finally{Date.now=realNow;}break;
    }
    case 'receipt':value=store.objectWriteReceipt(step.context,step.mutationId,step.hash);break;
    case 'contract':value=store.contract(step.key,step.version);break;
    case 'cleanup':store.transaction(()=>store.cleanupMutationReceipts(step.bytes??0,step.count??0));break;
    case 'adjust':store.adjustRetentionUsage(step.key,step.objects,step.revisions,step.bytes);break;
    case 'event':store.transaction(()=>store.appendCoreEvent(step.principal,step.objectId,step.revision,step.operation,step.mutationId,step.now,step.key));break;
    case 'admit':store.transaction(()=>store.admitEvent(step.reserve??false));break;
    case 'durableBoundary':value=store.durableEventBoundary();break;
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
