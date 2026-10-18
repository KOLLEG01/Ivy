import {readFileSync,existsSync} from 'node:fs';
import {stripTypeScriptTypes,registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
import {createServer,request} from 'node:http';
import {timingSafeEqual,randomUUID} from 'node:crypto';
import {schemas} from './data-collector-schema-oracle.mjs';
registerHooks({resolve(specifier,context,next){if(specifier.startsWith('.')&&specifier.endsWith('.js')&&context.parentURL){const source=new URL(specifier.slice(0,-3)+'.ts',context.parentURL);if(existsSync(fileURLToPath(source)))return next(source.href,context);}return next(specifier,context);}});
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {canonical,digest}=await import('../../packages/contracts/src/canonical.ts');
const sdk=/import\s*\{[^}]+\}\s*from\s*"[^\"]*packages\/sdk\/src\/node\.js";/;
const load=async(name,replacements)=>{let source=stripTypeScriptTypes(readFileSync(new URL('../../services/data-collector/src/'+name+'.ts',import.meta.url),'utf8'),{mode:'transform'});for(const [pattern,replacement]of replacements)source=source.replace(pattern,replacement);return import('data:text/javascript;base64,'+Buffer.from(source).toString('base64'));};
globalThis.__collectorHttp={IvyError,requireThat,canonical,digest,...schemas,scopedOperationId:()=>{throw Error('Unexpected remote mutation');}};
const {CollectorStore}=await load('store',[[sdk,'const {digest,requireThat}=globalThis.__collectorHttp;']]);
globalThis.__collectorHttp.CollectorStore=CollectorStore;
const {CollectorEngine}=await load('engine',[[sdk,'const {canonical,IvyError,requireThat,scopedOperationId}=globalThis.__collectorHttp;'],[/import\s*\{\s*CollectorStore\s*\}\s*from\s*".\/store.js";/,'const {CollectorStore}=globalThis.__collectorHttp;'],[/import\s*\{[^}]+\}\s*from\s*".\/schema.js";/,'const {effectiveRetention,runtimeReadSchema:readSchema,runtimeUpdateSchema:updateSchema}=globalThis.__collectorHttp;'],[/import\s*\{\s*TaskRunner\s*\}\s*from\s*".\/runner.js";/,'']]);
const {openMcpBridge}=await load('mcp-bridge',[[sdk,'const {IvyError}=globalThis.__collectorHttp;']]);
const main=stripTypeScriptTypes(readFileSync(new URL('../../services/data-collector/src/main.ts',import.meta.url),'utf8'),{mode:'transform'});
const ingressSource=main.slice(main.indexOf('const authorized ='),main.indexOf('const timer ='));
if(!ingressSource.includes('ingress.listen'))throw Error('Original handler boundary changed');
const openIngress=new Function('createServer','timingSafeEqual','randomUUID','IvyError','settings','store','engine',`return (async()=>{let closed=false;${ingressSource};return {server:ingress,close(){closed=true;ingress.closeAllConnections();return new Promise(r=>ingress.close(r));}};})();`);
const defaults={method:'POST',path:'/tasks/task/run',headers:['Authorization','Bearer synthetic','Idempotency-Key','run'],body:'{"ok":true}'};
const cases=[];const add=(name,changes={},kind='ingress')=>cases.push({name,kind,...defaults,...changes});
add('normal');add('bare-token',{headers:['Authorization','synthetic','Idempotency-Key','run']});
add('unauthorized',{headers:['Authorization','Bearer other']});add('missing-auth',{headers:[]});
add('lowercase-bearer',{headers:['Authorization','bearer synthetic']});add('wrong-method',{method:'GET'});
add('query-is-not-route',{path:'/tasks/task/run?x=1'});add('alias',{path:'/webhook'});
add('duplicate-auth-first',{headers:['Authorization','Bearer synthetic','Authorization','Bearer wrong','Idempotency-Key','run']});
add('duplicate-auth-wrong-first',{headers:['Authorization','Bearer wrong','Authorization','Bearer synthetic']});
add('duplicate-idempotency',{headers:['Authorization','Bearer synthetic','Idempotency-Key','first','Idempotency-Key','second']});
add('empty-idempotency',{headers:['Authorization','Bearer synthetic','Idempotency-Key','']});
add('disabled',{disabled:true});add('public-input',{public:true,headers:['Idempotency-Key','run']});
add('malformed',{body:'{' });add('null-input',{body:'null'});add('lone-surrogate',{body:'"\\ud800"'});
add('invalid-utf8',{bodyKind:'invalid'});add('exact-limit',{bodyKind:'large',size:262144});add('over-limit',{bodyKind:'large',size:262145});
add('chunked',{chunked:true});add('busy',{busy:true});add('idempotent',{repeat:true});
const mcp=(name,changes={})=>add(name,{path:'/call',headers:['Authorization','Bearer TOKEN'],body:'{"tool":"echo","arguments":{"ok":true}}',...changes},'mcp');
mcp('mcp-normal');mcp('mcp-wrong-path',{path:'/call?x=1'});mcp('mcp-wrong-auth',{headers:['Authorization','Bearer wrong']});mcp('mcp-method',{method:'GET'});
mcp('mcp-default-args',{body:'{"tool":"echo","arguments":null}'});mcp('mcp-missing-tool',{body:'{}'});mcp('mcp-null',{body:'null'});mcp('mcp-malformed',{body:'{' });
mcp('mcp-lone-tool',{body:'{"tool":"\\ud800","arguments":false}'});mcp('mcp-invalid-utf8',{bodyKind:'invalid-mcp'});
mcp('mcp-provider-error',{body:'{"tool":"fail"}'});mcp('mcp-large-result',{body:'{"tool":"large"}'});
mcp('mcp-exact-limit',{bodyKind:'large-mcp',size:1048576});mcp('mcp-over-limit',{bodyKind:'large-mcp',size:1048577});
mcp('mcp-cancelled',{cancelled:true});
function bytes(c){if(c.bodyKind==='invalid')return Buffer.from([34,0xed,0xa0,0x80,34]);if(c.bodyKind==='invalid-mcp')return Buffer.concat([Buffer.from('{"tool":"'),Buffer.from([0xed,0xa0,0x80]),Buffer.from('"}')]);if(c.bodyKind==='large')return Buffer.from('"'+'x'.repeat(c.size-2)+'"');if(c.bodyKind==='large-mcp'){const start='{"tool":"echo","arguments":"',end='"}';return Buffer.from(start+'x'.repeat(c.size-start.length-end.length)+end);}return Buffer.from(c.body);}
async function send(port,c,token){const body=bytes(c),headers=c.headers.map(v=>v.replace('TOKEN',token??'TOKEN'));headers.push('Host','localhost','Connection','close');if(!c.chunked)headers.push('Content-Length',String(body.length));return new Promise((resolve,reject)=>{const req=request({host:'127.0.0.1',port,method:c.method,path:c.path,headers},res=>{const parts=[];res.on('data',d=>parts.push(d));res.on('end',()=>{const text=Buffer.concat(parts).toString('utf8');resolve({status:res.statusCode,body:text?JSON.parse(text):null});});});req.on('error',reject);if(c.chunked){req.write(body.subarray(0,3));req.end(body.subarray(3));}else req.end(body);});}
const results=[];
for(const [index,c]of cases.entries()){
 let server,store,engine,token,port,calls=0,closed=0;
 if(c.kind==='ingress'){
  store=new CollectorStore(process.argv[3]+'-'+index+'.sqlite');
  const settings=schemas.settingsSchema.parse({secrets:{INPUT:'synthetic'},ingress:{host:'127.0.0.1',port:1,routes:{'/webhook':'task'}}});settings.ingress.port=0;
  engine=new CollectorEngine(store,{clearAuthentication(){}},settings);
  const task={id:'task',name:'Task',enabled:!c.disabled,intervalSeconds:60,timeoutSeconds:5,memoryMb:64,script:'export default()=>({data:1})',dependencies:{},config:{},secretNames:[],inputSecretName:'INPUT',allowUnauthenticatedInput:!!c.public,retention:schemas.defaultRetention};
  if(c.public)task.inputSecretName=null;
  engine.update({action:'save',operationId:'save',expectedRevision:0,task});
  if(c.busy)engine.update({action:'run',operationId:'prior',id:'task'});
  server=await openIngress(createServer,timingSafeEqual,randomUUID,IvyError,settings,store,engine);port=server.server.address().port;
 }else{
  const controller=new AbortController();if(c.cancelled)controller.abort();
  server=await openMcpBridge({async call(tool,args){calls++;if(tool==='fail')throw new IvyError('provider_unavailable','private diagnostic','unknown');if(tool==='large')return 'x'.repeat(1048576);return {tool,args};},async close(){closed++;}},controller.signal);
  token=server.context.token;port=new URL(server.context.url).port;
 }
 try{let response=await send(port,c,token);if(c.repeat)response=await send(port,c,token);const queued=store?store.runs('queued').map(r=>({id:r.id,input:r.input})):null;results.push({case:c,response,queued,calls});}
 finally{await server.close();store?.close();}
}
if(results.find(r=>r.case.name==='normal').response.status!==202||results.find(r=>r.case.name==='mcp-normal').response.status!==200)throw Error('Oracle did not reach the original application handlers');
process.stdout.write(JSON.stringify(results));
