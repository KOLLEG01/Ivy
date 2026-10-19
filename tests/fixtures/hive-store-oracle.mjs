import {registerHooks,stripTypeScriptTypes,syncBuiltinESMExports} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import crypto from 'node:crypto';
import {join,relative} from 'node:path';
import {setup as uiFileSetup,tree as uiFileTree} from './hive-ui-files-fixture.mjs';
registerHooks({resolve(specifier,context,next){
  if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}
  return next(specifier,context);
},load(url,context,next){
  if(url.endsWith('.ts'))return{format:'module',source:stripTypeScriptTypes(readFileSync(new URL(url),'utf8'),{mode:'transform'}),shortCircuit:true};
  return next(url,context);
}});
const {HiveStore}=await import('../../services/hive/src/store.ts');
const {Objects}=await import('../../services/hive/src/objects.ts');
const {queryObjects}=await import('../../services/hive/src/query.ts');
const {Events}=await import('../../services/hive/src/events.ts');
const {Retention}=await import('../../services/hive/src/retention.ts');
const {pruneObjectRevisions}=await import('../../services/hive/src/object-retention.ts');
const {Registry}=await import('../../services/hive/src/registry.ts');
const {Inventory}=await import('../../services/hive/src/inventory.ts');
const {Configurations}=await import('../../services/hive/src/configurations.ts');
const {UiFiles}=await import('../../services/hive/src/ui-files.ts');
const {assetPath}=await import('../../services/hive/src/uis.ts');
const {uiContentPolicy}=await import('../../services/hive/src/ui-security.ts');
const {wikiHandlers}=await import('../../services/hive/src/wiki.ts');
const {validateInput,validateOutput}=await import('../../packages/contracts/src/core-validation.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {canonical,digest,hashJson}=await import('../../packages/contracts/src/canonical.ts');
// Invoke the unchanged worker functions in isolation with the actual HiveStore.
// No complete kernel, transport or startup lifecycle is supplied by this fixture.
const workerSource=readFileSync(new URL('../../services/hive/src/worker.ts',import.meta.url),'utf8');
const identityStart=workerSource.indexOf('const oauthPrefix = '),identityEnd=workerSource.indexOf('let chain = Promise.resolve();');
if(identityStart<0||identityEnd<identityStart)throw Error('Original worker identity slice changed');
const identitySource=stripTypeScriptTypes(workerSource.slice(identityStart,identityEnd),{mode:'strip'});
const originalIdentity=new Function('kernel','canonical','digest','createHmac','randomBytes','IvyError','requireThat',identitySource+'\nreturn {handle,collectOAuthFamilies,credentialReference};');
const input=JSON.parse(readFileSync(0,'utf8'));
let store,registry,discovery,configurations,Discovery,directDescriptors,directCall,hasStructuredOutput,McpSchemaBundle,mcpSurfaceBindings,mcpSuccess,mcpRequest;
const registryTrace=input.steps.some(step=>['registry','inventory','discovery','configurations'].includes(step.op));
const inventoryTrace=input.steps.some(step=>step.op==='inventory');
const discoveryTrace=input.steps.some(step=>step.op==='discovery');
const plainLowercase=new Map(),uppercase=new Map(),englishStrings=new Set(),wordCharacters=new Map();
const realPlainLower=String.prototype.toLowerCase,realUpper=String.prototype.toUpperCase,realCompare=String.prototype.localeCompare;
if(discoveryTrace){
 String.prototype.toLowerCase=function(...args){const value=realPlainLower.apply(this,args);plainLowercase.set(String(this),value);for(const char of value)wordCharacters.set(char.codePointAt(0),/[\p{L}\p{N}]/u.test(char));return value;};
 String.prototype.localeCompare=function(other,...args){if(args[0]==='en'){englishStrings.add(String(this));englishStrings.add(String(other));}return realCompare.call(this,other,...args);};
 String.prototype.toUpperCase=function(...args){const value=realUpper.apply(this,args);uppercase.set(String(this),value);return value;};
 ({Discovery}=await import('../../services/hive/src/discovery.ts'));
 ({directDescriptors,directCall,hasStructuredOutput}=await import('../../services/hive/src/mcp-catalog.ts'));
 ({McpSchemaBundle}=await import('../../services/hive/src/mcp-schema.ts'));
 ({request:mcpRequest}=await import('./hive-mcp-handlers.mjs'));
 const facade=readFileSync(new URL('../../services/hive/src/mcp.ts',import.meta.url),'utf8');
 const surfaceStart=facade.indexOf('export function mcpSurfaceBindings('),surfaceEnd=facade.indexOf('type CursorAccess = ',surfaceStart),successStart=facade.indexOf('export function mcpSuccess('),successEnd=facade.indexOf('function failure(',successStart);
 if(surfaceStart<0||surfaceEnd<surfaceStart||successStart<0||successEnd<successStart)throw Error('Original MCP projection slices changed');
 ({mcpSurfaceBindings,mcpSuccess}=new Function(stripTypeScriptTypes(facade.slice(surfaceStart,surfaceEnd).replace('export function','function')+facade.slice(successStart,successEnd).replace('export function','function'),{mode:'strip'})+';return {mcpSurfaceBindings,mcpSuccess};')());
}
const lowercase=new Map();
const localeStrings=new Set();
const searchWordCharacters=new Map();
const observeSearch=params=>{if(typeof params.text==='string')for(const character of params.text)searchWordCharacters.set(character.codePointAt(0),/[\p{L}\p{N}]/u.test(character));};
const units=value=>Array.from({length:String(value).length},(_,i)=>String(value).charCodeAt(i));
const collectStrings=value=>{
 if(typeof value==='string')localeStrings.add(value);
 else if(Array.isArray(value))for(const child of value)collectStrings(child);
 else if(value&&typeof value==='object')for(const child of Object.values(value))collectStrings(child);
};
const diagnosticString=value=>{
 collectStrings(value);
 try{localeStrings.add(hashJson({code:value.code,resource:value.resource,source:value.source}));}catch{}
};
function registryStrings(step){
 collectStrings(step.params);collectStrings(step.context);
 for(const row of store.all('SELECT identity,diagnostic_json FROM diagnostics')){
  localeStrings.add(row.identity);try{diagnosticString(JSON.parse(row.diagnostic_json));}catch{}
 }
 for(const row of store.all('SELECT status_json FROM service_nodes')){try{
  const value=JSON.parse(row.status_json);collectStrings(value);
  diagnosticString({code:'registry_invalid',resource:{serviceNodeId:value.serviceNodeId},source:'hive'});
 }catch{}}
 if(step.method==='diagnostic')diagnosticString(step.params);
 if(step.method==='heartbeat')for(const value of step.params?.diagnostics??[])diagnosticString({...value,source:'service:'+step.id});
}
// Observe only the runtime primitives. Rust executes retention policy and SQL
// independently; these values do not supply selected candidates or results.
const retentionRuntime={dates:new Map(),iso:new Map(),comparisons:new Map()};
function retentionDate(RealDate,now){
  const remember=value=>{retentionRuntime.dates.set(value,RealDate.parse(value));};
  for(const row of store.all('SELECT created_at,updated_at FROM objects')){remember(row.created_at);remember(row.updated_at);}
  for(const row of store.all('SELECT created_at FROM revisions'))remember(row.created_at);
  const keys=[];
  for(const row of store.all('SELECT definition FROM contracts')){try{const key=JSON.parse(row.definition)?.key;if(typeof key==='string'){keys.push(key);if(registryTrace)localeStrings.add(key);}}catch{}}
  for(const a of keys)for(const b of keys)retentionRuntime.comparisons.set(JSON.stringify([a,b]),Math.sign(a.localeCompare(b)));
  return class extends RealDate{
    constructor(...args){super(...(args.length?args:[now]));if(typeof args[0]==='string')remember(args[0]);}
    static now(){return now;}
    static parse(value){remember(value);return RealDate.parse(value);}
    toISOString(){const result=super.toISOString();retentionRuntime.iso.set(this.getTime(),result);return result;}
  };
}
const tableHash=(table,columns)=>'sha256:'+createHash('sha256').update(store.get(`SELECT json_group_array(json_array(${columns})) AS encoded FROM (SELECT rowid AS record_id,* FROM ${table} ORDER BY rowid)`).encoded).digest('hex');
const snapshot=(compactEvents=false,compactDiagnostics=false)=>({contracts:tableHash('contracts','key,version,definition'),families:store.all('SELECT * FROM contract_families ORDER BY key'),objects:store.all('SELECT * FROM objects ORDER BY id'),fts:store.all('SELECT rowid,object_id,name,text FROM object_fts ORDER BY rowid'),revisions:tableHash('revisions',"record_id,object_id,revision,contract_key,contract_version,encoding,CASE WHEN typeof(content)='blob' THEN hex(content) ELSE content END,content_hash,byte_length,created_at,references_json"),references:store.all('SELECT * FROM revision_references ORDER BY source_object_id,source_revision,name'),principals:store.all('SELECT * FROM principals ORDER BY id'),credentials:store.all('SELECT * FROM credentials ORDER BY digest'),nodes:store.all('SELECT * FROM service_nodes ORDER BY id'),sessions:store.all('SELECT * FROM browser_sessions ORDER BY digest'),subscriptions:store.all('SELECT * FROM subscriptions ORDER BY node_id,name'),durable:store.all('SELECT * FROM durable_subscriptions ORDER BY node_id,name'),changes:[...store.changes],mutations:tableHash('mutations','record_id,principal_id,mutation_id,request_hash,issued_at_ms,completed_at_ms,result_kind,result_json,receipt_bytes'),events:tableHash('events',compactEvents?'sequence,topic,topic_version,source,occurred_at,mutation_id,length(CAST(payload AS BLOB))':'sequence,topic,topic_version,source,occurred_at,mutation_id,payload'),metadata:store.all("SELECT * FROM metadata WHERE key NOT IN ('cursor_secret','runtime_epoch') ORDER BY key"),retention:store.all('SELECT * FROM retention_families ORDER BY contract_key'),apps:store.all('SELECT * FROM apps ORDER BY app_id'),releases:store.all('SELECT * FROM app_releases ORDER BY app_id,release_id'),diagnostics:compactDiagnostics?tableHash('diagnostics','record_id,identity,diagnostic_json'):store.all('SELECT * FROM diagnostics ORDER BY identity'),connections:store.all('SELECT * FROM service_connections ORDER BY node_id'),managed:store.all('SELECT * FROM managed_nodes ORDER BY node_id'),hosts:store.all('SELECT * FROM host_reports ORDER BY host_id'),registries:store.all('SELECT * FROM registries ORDER BY node_id'),inventorySchemas:store.all('SELECT * FROM inventory_schemas ORDER BY namespace,kind,version'),toolDefinitions:store.all('SELECT * FROM tool_definitions ORDER BY hash')});
const rows=[];
try {
 store=new HiveStore(input.path);
 for(const original of input.steps){
  const step=original.bindings||original.identityBindings?structuredClone(original):original;
  const set=(value,pointer,next)=>{const fields=pointer.split('/').slice(1).map(s=>s.replaceAll('~1','/').replaceAll('~0','~'));for(const field of fields.slice(0,-1))value=value[field];value[fields.at(-1)]=next;};
  for(const binding of step.bindings??[])set(step,binding.at,rows[binding.from].value.object.id);
  for(const binding of step.identityBindings??[]){let v=rows[binding.from];for(const key of binding.select.split('/').slice(1))v=v[key];set(step,binding.at,binding.digest?digest(v):v);}
  let value=null,error=null,wikiCalls=[];
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
    case 'identity':{
      const RealDate=Date;
      if(new RealDate(step.now).toISOString()!==step.nowText)throw Error('Fixture clock mismatch');
      globalThis.Date=class extends RealDate{constructor(...args){super(...(args.length?args:[step.now]));}static now(){return step.now;}};
      try{
        const api=originalIdentity({get store(){return store;},execute(_context,request){return {forwarded:request};}},canonical,digest,crypto.createHmac,
          size=>step.sessionBytes?Buffer.from(step.sessionBytes,'base64'):crypto.randomBytes(size),IvyError,requireThat);
        if(step.method==='credentialReference')value=api.credentialReference(step.params.digest);
        else if(step.method==='collect')value=store.transaction(()=>api.collectOAuthFamilies(step.now,step.params.active===undefined?undefined:new Set(step.params.active)));
        else value=await api.handle({action:step.method,...step.params});
      }finally{globalThis.Date=RealDate;}
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
    case 'pruneRevisions':case 'retention':{
      const RealDate=Date;
      globalThis.Date=retentionDate(RealDate,step.now);
      try{
        if(step.op==='pruneRevisions'){
          const context={...store.authenticate(step.context),serviceNodeId:step.context.serviceNodeId,generation:1};
          value=store.withBudget(()=>pruneObjectRevisions(store,context,step.params),step.overdue?-1:10000);
        }else{
          const diagnostics=step.diagnosticSql||step.diagnosticFail?{collectDiagnostics(){store.db.exec(step.diagnosticSql??'');if(step.diagnosticFail)throw new IvyError('fixture_failure','Diagnostic collection requested failure.');}}:undefined;
          const retention=new Retention(store,diagnostics);
          if(step.method==='recordFailure'){
            const message=step.messageUnits?String.fromCharCode(...step.messageUnits):step.message;
            retention.recordFailure(new Error(message),step.now);
          }else value=retention[step.method](step.now);
        }
      }finally{globalThis.Date=RealDate;}
      break;
    }
    case 'search':{
      const params=structuredClone(step.params??{});
      for(const [pointer,codeUnits]of Object.entries(step.parameterUnits??{}))set(params,pointer,String.fromCharCode(...codeUnits));
      if(step.method==='cursor')value=store.cursor(params.identity,params.boundary);
      else{observeSearch(params);value=store.withBudget(()=>new Objects(store).search(params),step.overdue?-1:10000);}
      break;
    }
    case 'wiki':{
      const RealDate=Date,realUUID=crypto.randomUUID;
      const params=structuredClone(step.params??{});
      for(const [pointer,codeUnits]of Object.entries(step.parameterUnits??{}))set(params,pointer,String.fromCharCode(...codeUnits));
      globalThis.Date=retentionDate(RealDate,step.now);
      if(step.createdId){crypto.randomUUID=()=>step.createdId;syncBuiltinESMExports();}
      try{
        validateInput(step.method,params);
        const context=store.authenticate(step.context),objects=new Objects(store);
        const called=(method,params)=>{wikiCalls.push({method,params:structuredClone(params)});if(step.failAt===wikiCalls.length)throw new IvyError('fixture_failure','Requested Wiki dependency failure.');};
        const request=(method,params)=>{
          called(method,params);validateInput(method,params);
          if(method==='objects.search')observeSearch(params);
          const result=method==='objects.query'?queryObjects(store,params):method==='objects.write'?objects.write(context,params):method==='objects.move'?objects.move(context,params):method==='objects.archive'?objects.archive(context,params):objects[method.slice(8)](params);
          validateOutput(method,result);return result;
        };
        const update=params=>{called('wiki.updateContent',params);return objects.write(context,{mutationId:params.mutationId,objectId:params.objectId,expectedRevision:params.expectedRevision,contractVersion:'1.0.0',references:{},content:{encoding:'text',value:params.markdown}},true);};
        value=store.withBudget(()=>wikiHandlers(request,update)[step.method](params),step.overdue?-1:10000);
        validateOutput(step.method,value);
      }finally{globalThis.Date=RealDate;crypto.randomUUID=realUUID;syncBuiltinESMExports();}
      break;
    }
    case 'uiFiles':{
      const root=input.path.replace(/\.[^.\\/]+$/,'')+'.files',files=new UiFiles(join(root,'private'));
      const params=structuredClone(step.params??{});
      for(const [pointer,codeUnits]of Object.entries(step.parameterUnits??{}))set(params,pointer,String.fromCharCode(...codeUnits));
      const path=value=>relative(root,value).replaceAll('\\','/');
      if(step.prepareSql)store.db.exec(step.prepareSql);
      switch(step.method){
        case 'fixture':uiFileSetup(root,params.actions);break;
        case 'assetPath':value=assetPath(params.path);break;
        case 'contentPolicy':value=uiContentPolicy(params.path);break;
        case 'directory':value=path(files.directory(params.uiId,params.releaseId));break;
        case 'path':value=path(files.path(params.uiId,params.releaseId,params.path));break;
        case 'available':value=files.available(params.uiId,params.releaseId,params.asset);break;
        case 'check':value=files.check(params.uiId,params.releaseId,params.asset);break;
        case 'stage':files.stage(params.uiId,params.releaseId,params.asset,Buffer.from(params.data,'base64'));break;
        case 'promote':files.promote(params.uiId,params.releaseId,params.assets);break;
        case 'read':value=(await files.read(params.uiId,params.releaseId,params.asset)).toString('base64');break;
        case 'install':await files.install(join(root,'bundle'),params.uiId,params.releaseId,params.assets);break;
        case 'collect':files.collect(params.uiId,new Set(params.retained??[]),step.now);break;
        case 'collectTemporary':files.collectTemporary(step.now);break;
        case 'collectAll':{
          const realNow=Date.now;let call=0;Date.now=()=>step.clock?.[call++]??step.now;
          try{files.collectAll(new Map(Object.entries(params.retained??{}).map(([id,ids])=>[id,new Set(ids)])));}finally{Date.now=realNow;}
          break;
        }
        case 'migrateLegacy':files.migrateLegacy(store);break;
        default:throw Error('Unknown UI file method');
      }
      if(step.lossless)value={encoded:JSON.stringify(value)};
      break;
    }
    case 'configurations':{
      const RealDate=Date,realUUID=crypto.randomUUID;
      const params=structuredClone(step.params??{});
      for(const [pointer,codeUnits]of Object.entries(step.parameterUnits??{}))set(params,pointer,String.fromCharCode(...codeUnits));
      registryStrings({...step,params});globalThis.Date=retentionDate(RealDate,step.now);
      if(step.createdId){crypto.randomUUID=()=>step.createdId;syncBuiltinESMExports();}
      try{
        if(!configurations||step.method==='constructor')configurations=new Configurations(store);
        const context=step.context?store.authenticate(step.context):null;
        value=store.withBudget(()=>{
          switch(step.method){
            case 'constructor':return null;
            case 'get':return configurations.get(params.hostId,params.revision);
            case 'list':return configurations.list();
            case 'put':return configurations.put(context,params);
            case 'edit':return configurations.edit(params.hostId,params.revision);
            case 'save':return configurations.save(context,params);
            case 'history':return configurations.history(params);
            case 'projectRead':return configurations.projectRead(params.read??new Objects(store).read(params));
            case 'isConfigurationContract':return configurations.isConfigurationContract(params.key);
            case 'sql':store.run(params.sql,...(params.bindings??[]));return null;
            default:throw Error('Unknown configuration method');
          }
        },step.overdue?-1:10000);
        if(step.lossless)value={encoded:JSON.stringify(value)};
      }finally{globalThis.Date=RealDate;crypto.randomUUID=realUUID;syncBuiltinESMExports();registryStrings({...step,params});}
      break;
    }
    case 'registry':case 'inventory':case 'discovery':{
      const RealDate=Date;
      const realLower=String.prototype.toLocaleLowerCase;
      const params=step.params===undefined?{}:step.params;
      for(const [pointer,codeUnits]of Object.entries(step.parameterUnits??{}))set(params,pointer,String.fromCharCode(...codeUnits));
      if(step.idUnits)step.id=String.fromCharCode(...step.idUnits);
      registryStrings({...step,params});
      globalThis.Date=retentionDate(RealDate,step.now);
      if(step.op==='inventory')String.prototype.toLocaleLowerCase=function(...args){const value=realLower.apply(this,args);lowercase.set(String(this),value);return value;};
      try{
        if((step.method==='constructor'&&step.op!=='discovery')||!registry)registry=new Registry(store);
        const context={...(step.context?store.authenticate(step.context):{}),serviceNodeId:step.id,generation:step.generation};
        if(step.op==='discovery'){
          if(!discovery||step.method==='constructor')discovery=new Discovery(registry);
          switch(step.method){
            case 'constructor':break;
            case 'list':value=discovery.list(params,step.mcpOnly??false);break;
            case 'describe':value=discovery.describe(params,step.mcpOnly??false);break;
            case 'resolveCall':value=discovery.resolveCall(params);break;
            case 'mcpCatalog':value=discovery.mcpCatalog(params.name);break;
            case 'instructions':value=discovery.instructions();break;
            case 'instructionsPage':value=discovery.instructionsPage(params);break;
            case 'toolSchema':value=discovery.toolSchema(params);break;
            case 'mcpSurface':value=mcpSurfaceBindings(params.bindings??discovery.mcpCatalog(params.name),params.surface);break;
            case 'mcpSuccess':value=mcpSuccess(params.result,params.structured);break;
            case 'mcpRequest':value=await mcpRequest(params,{store,discovery,hashJson,IvyError,requireThat,directCall,directDescriptors,hasStructuredOutput});break;
            case 'directDescriptors':case 'directCall':case 'hasStructuredOutput':{
              let bindings=params.bindings??discovery.mcpCatalog(params.name);
              if(params.surface)bindings=bindings.filter(b=>b.definition.discovery?.mcp?.surface===params.surface);
              value=step.method==='directDescriptors'?directDescriptors(bindings):step.method==='directCall'?directCall(bindings,params.args):hasStructuredOutput(bindings);break;
            }
            case 'mcpBundle':{
              const bundle=new McpSchemaBundle(),embeds=[];for(const entry of params.entries)embeds.push(bundle.embed(entry.schema,entry.path));
              value={embeds,finished:bundle.finish(params.root??{type:'object',properties:{}})};break;
            }
            case 'cursor':value=store.cursor(params.identity,params.boundary);break;
            default:throw Error('Unknown discovery method');
          }
        }else if(step.op==='inventory'){
          const inventory=new Inventory(store,registry);
          value=store.withBudget(()=>{
            if(step.method==='sync')return inventory.sync(context,params);
            if(step.method==='get')return inventory.get(params.resourceRef);
            if(step.method==='list')return inventory.list(params);
            if(step.method==='cursor')return store.cursor(params.identity,params.boundary);
            if(step.method==='sql')return store.all(step.sql,...(params.bindings??[]));
            throw Error('Unknown inventory method');
          },step.overdue?-1:10000);
        }else switch(step.method){
          case 'constructor':break;
          case 'connect':value=registry.connect({...context,sessionDigest:step.context?.sessionDigest},params);break;
          case 'disconnect':registry.disconnect(step.id);break;
          case 'node':value=registry.node(step.id);break;
          case 'nodes':value=registry.nodes();break;
          case 'save':registry.save(params,step.notify??true);break;
          case 'sync':value=registry.sync(context,params);break;
          case 'heartbeat':value=registry.heartbeat(context,params);break;
          case 'diagnostic':registry.diagnostic(params);break;
          case 'diagnostics':value=registry.diagnostics(params.status);break;
          case 'collectDiagnostics':registry.collectDiagnostics(step.now);break;
          case 'checkRequirements':registry.checkRequirements(params.requirements,{allowUnregistered:params.allowUnregistered});break;
          case 'getRegistry':value=registry.getRegistry(step.id);break;
          case 'namespace':value=registry.namespace(step.id,params.namespace);break;
          case 'select':value=registry.select(params.namespace,params.target);break;
          case 'snapshot':value=registry.snapshot(params.namespace,params.target);break;
          case 'bindings':value=registry.bindings(step.id,params.namespace);break;
          case 'liveCatalog':value=registry.liveCatalog(step.id);break;
          case 'reportHost':value=registry.reportHost(context,params);break;
          case 'hostObservations':value=registry.hostObservations(params.hostId);break;
          case 'clearHints':registry.changedNodes.clear();registry.changedGenerations.clear();store.changes.clear();break;
          case 'collectRetention':value=new Retention(store,registry).collect(step.now);break;
          default:throw Error('Unknown registry method');
        }
        if(step.lossless)value={encoded:JSON.stringify(value)};
      }finally{globalThis.Date=RealDate;String.prototype.toLocaleLowerCase=realLower;registryStrings({...step,params});}
      break;
    }
    case 'configure':store.configureCredentials(step.credentials);break;
    case 'authenticate':value=store.authenticate(step.context);break;
    case 'sql':store.db.exec(step.sql);break;
    case 'invalidate':store.invalidate(step.scope);break;
    case 'transaction':store.transaction(()=>{store.db.exec(step.sql);store.invalidate(step.scope);if(step.fail)throw new IvyError('fixture_failure','Rollback requested.');});break;
    case 'reopen':store.close();store=new HiveStore(input.path);registry=undefined;discovery=undefined;configurations=undefined;break;
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
  const state=snapshot(step.compactEvents??false,step.compactDiagnostics??false);
  if(step.op==='uiFiles')state.appAssets=store.all('SELECT * FROM app_assets ORDER BY app_id,release_id,path');
  if(inventoryTrace){state.inventorySets=store.all('SELECT * FROM inventory_sets ORDER BY node_id,namespace,kind');state.inventory=step.compactInventory?tableHash('inventory','record_id,node_id,namespace,kind,native_id,schema_version,summary,observed_at'):store.all('SELECT * FROM inventory ORDER BY node_id,namespace,kind,native_id');}
  if(step.lossless)state.changes={encoded:JSON.stringify(state.changes)};
  const mirror=registryTrace?{registry:registry?{changedNodes:[...registry.changedNodes].map(units),changedGenerations:[...registry.changedGenerations].map(([id,g])=>[units(id),g])}:null}:{};
  rows.push({value,error,state,...mirror,...(step.op==='wiki'?{wikiCalls}:{}),...(step.op==='uiFiles'?{fileTree:uiFileTree(input.path.replace(/\.[^.\\/]+$/,'')+'.files')}:{})});
 }
 const definitions=store.all("SELECT type,name,tbl_name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name");
 const metadata=store.all("SELECT * FROM metadata WHERE key NOT IN ('cursor_secret','runtime_epoch') ORDER BY key");
 const topics=store.all('SELECT * FROM topics ORDER BY topic,version');
 const runtime=input.steps.some(step=>['retention','pruneRevisions'].includes(step.op))?{retentionRuntime:Object.fromEntries(Object.entries(retentionRuntime).map(([key,map])=>[key,[...map]]))}:{};
 if(input.steps.some(step=>['search','wiki'].includes(step.op)))runtime.searchRuntime={wordCharacters:[...searchWordCharacters]};
 if(registryTrace){
  const sorted=[...localeStrings].sort((a,b)=>a.localeCompare(b));let rank=0;
  const localeOrder=sorted.map((value,index)=>{if(index&&value.localeCompare(sorted[index-1])!==0)rank++;return [units(value),rank];});
  runtime.registryRuntime={dates:[...retentionRuntime.dates].map(([value,date])=>[units(value),date]),iso:[...retentionRuntime.iso],localeOrder};
  if(inventoryTrace)runtime.registryRuntime.lowercase=[...lowercase].map(([value,lower])=>[units(value),units(lower)]);
  if(discoveryTrace){
   const sorted=[...englishStrings].sort((a,b)=>realCompare.call(a,b,'en'));let rank=0;
   runtime.registryRuntime.discovery={lowercase:[...plainLowercase].map(([v,l])=>[units(v),units(l)]),uppercase:[...uppercase].map(([v,l])=>[units(v),units(l)]),wordCharacters:[...wordCharacters],englishOrder:sorted.map((v,i)=>{if(i&&realCompare.call(v,sorted[i-1],'en')!==0)rank++;return [units(v),rank];})};
  }
 }
 process.stdout.write(JSON.stringify({rows,definitions,metadata,topics,...runtime}));
}finally{store?.close();String.prototype.toLowerCase=realPlainLower;String.prototype.toUpperCase=realUpper;String.prototype.localeCompare=realCompare;}
