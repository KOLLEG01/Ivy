import {readFileSync,readdirSync,existsSync} from 'node:fs';
import {registerHooks,createRequire} from 'node:module';import {fileURLToPath,pathToFileURL} from 'node:url';
const require=createRequire(import.meta.url);
const external=new Map(['ajv/dist/2020.js','re2js'].map(name=>[name,pathToFileURL(require.resolve(name)).href]));
registerHooks({resolve(s,c,n){if(external.has(s))return n(external.get(s),c);if(s.startsWith('.')&&s.endsWith('.js')){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {NativeContract,nativeToolDefinition,isReadOnlyNativeMethod}=await import('../../packages/contracts/src/native-contract.ts');
const {IvyError}=await import('../../packages/contracts/src/errors.ts');
const {hashJson,digest}=await import('../../packages/contracts/src/canonical.ts');
const summary=v=>({hash:hashJson(v),bytes:Buffer.byteLength(JSON.stringify(v)),orderedHash:digest(JSON.stringify(v))});
const capture=action=>{try{return {ok:true,value:action()};}catch(e){return {ok:false,error:IvyError.from(e).toWire()};}};
const rows=[];
function collect(catalog,source){
 const constructed=capture(()=>new NativeContract(catalog));
 if(!constructed.ok){rows.push({...source,constructed});return;}
 const contract=constructed.value,methods=[];
 for(const method of catalog.clientRequests){const name=method.method,definition=contract.definition(name);
  const schemas=[[],['threadId','input','clientUserMessageId'],['b','a','a','\ud800','😀'],...(source.label?[['length','0']]:[])].map(reserved=>({reserved:JSON.stringify(reserved),result:capture(()=>summary(contract.inputSchema(name,reserved)))}));
  const originalHash=contract.definitionHash(name),decorated={...definition,discovery:{group:'codex/turns',summary:'Read retained data.'}},changed={...definition,description:'changed'};
  const variants=[['original',definition,originalHash],['decorated',decorated,originalHash],['changed',changed,hashJson(changed)],['wrongHash',definition,'sha256:'+'0'.repeat(64)],['invalid',{},originalHash]];
  methods.push({name:JSON.stringify(name),readOnly:isReadOnlyNativeMethod(name),definition:summary(definition),definitionHash:originalHash,projection:summary(nativeToolDefinition(catalog,method,methods.length===0?undefined:contract.catalogHash)),schemas,verify:variants.map(([kind,value,hash])=>({kind,result:capture(()=>{contract.verifyDefinition(name,value,hash);return null;})}))});
 }
 rows.push({...source,constructed:{ok:true,value:summary(contract.catalog)},methods,unknown:capture(()=>contract.definition('unrecognized/method')),unknownHash:capture(()=>contract.definitionHash('unrecognized/method'))});
}
for(const dir of readdirSync('specs/native').filter(s=>/^codex-/.test(s)))for(const file of readdirSync('specs/native/'+dir).filter(s=>/^catalog(?:\.[a-z0-9]+-[a-z0-9]+)?\.json$/.test(s))){const path='specs/native/'+dir+'/'+file;collect(JSON.parse(readFileSync(path,'utf8')),{path});}
const catalog=JSON.parse(readFileSync('specs/native/codex-0.159.2/catalog.json','utf8'));
const method={...catalog.clientRequests[0],method:'thread/read',inputSchema:true,outputSchema:true};
const base={...catalog,clientRequests:[method],serverRequests:[],serverNotifications:[],clientNotifications:[],derivedLegacyTypes:[]};
const schemas=[true,false,{}, {type:'object',properties:{title:{type:'string',description:'annotation'},description:{title:'annotation'},$id:{type:'string'},threadId:{type:'string'},extension:{type:'object',additionalProperties:true}},required:['title','description','$id','threadId'],description:'drop',default:{title:'keep',$id:'keep'}},
 {$ref:'#/$defs/Root',$defs:{Root:{type:'object',properties:{threadId:{type:'string'},keep:{title:'drop'}},required:['threadId','keep']}}},
 {anyOf:[{$ref:'#/$defs/Root'},{type:'null'}],$defs:{Root:{type:'object',properties:{extension:{type:'object',additionalProperties:true}}}}},
 {$ref:'#/$defs/Root',$defs:{Root:{$ref:'#/$defs/Root'}}},
 {oneOf:[{$ref:'#/$defs/A'}],$defs:{A:{anyOf:[{$ref:'#/$defs/B'},{type:'object',properties:{x:{}}}]},B:{$ref:'#/$defs/A'}}},
 // Malformed dictionaries/alternatives have native admission cases in native-contract.rs.
 {allOf:[{type:'object',properties:{x:{}}}],default:{description:'keep'},examples:[{$id:'keep'}]},
 {$ref:'#/$defs/Absent',$defs:{}},{$ref:'external.json#/$defs/A'},{$ref:'#/$defs/a_b',$defs:{a_b:{}}},{$ref:3},
 // Missing/prototype/array definitions are covered by the same native cases.
 {$ref:'#/$defs/Root',$defs:{Root:false}},{$ref:'#/$defs/Root',$defs:{Root:{$ref:'#/$defs/Absent'}}},
 ];
for(let i=0;i<schemas.length;i++){const c={...base,clientRequests:[{...method,inputSchema:schemas[i]}]};collect(c,{label:'schema-'+i,source:JSON.stringify(c)});}
for(const name of ['constructor','__proto__','TURN/START','thread/start','fs/readFile','thread/items/list','\ud800','😀']){const c={...base,clientRequests:[{...method,method:name}]};collect(c,{label:'method-name',source:JSON.stringify(c)});}
for(const c of [null,{}, {...base,clientRequests:[method,method]},{...base,version:''},{...base,clientRequests:[{...method,inputSchema:null}]}])collect(c,{label:'invalid-catalog',source:JSON.stringify(c)});
process.stdout.write(JSON.stringify(rows));
