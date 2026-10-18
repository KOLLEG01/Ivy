import {registerHooks} from 'node:module';
import {readFileSync} from 'node:fs';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import assert from 'node:assert/strict';
const hook=registerHooks({load(url,context,next){
  if(url.endsWith('/dist/services/agent-manager/src/environment.js'))return {format:'module',source:readFileSync(new URL(url),'utf8')+'\nexport {renderMcpSections};\n',shortCircuit:true};
  return next(url,context);
}});
const {renderMcpSections}=await import('../../dist/services/agent-manager/src/environment.js');hook.deregister();
const capture=run=>{try{return {result:run()};}catch(error){return {error:IvyError.from(error).toWire()};}};
const resourceUrls=hive=>[...new Set(['/mcp','/mcp-dev'].map(path=>new URL(hive.replace(/\/$/,'')+path).href))];
const bases=['https://hive.example','https://HIVE.example:443/','http://LOCALHOST:80/base/','https://hive.example//','https://hive.example?q=','https://hive.example#fragment','https://bücher.example','https://hive.example/\ud800','relative',''];
const resources=bases.map(hive=>({hive,expected:capture(()=>resourceUrls(hive))}));
const urls=['https://hive.example/mcp','https://HIVE.example:443/a/../mcp','https://hive.example/mcp-dev','https://hive.example/mcp/','https://hive.example/mcp#','https://hive.example/mcp?','https://hive.example/mcp#fragment','https://hive.example/mcp?query','https://:@hive.example/mcp','https://user@hive.example/mcp','https://:pass@hive.example/mcp','https://other.example/mcp','http://hive.example/mcp','http://localhost/mcp','http://LOCALHOST:80/mcp','http://localhost./mcp','http://127.1/mcp','http://2130706433/mcp','http://[0:0:0:0:0:0:0:1]/mcp','http://[::ffff:127.0.0.1]/mcp','https://bücher.example/😀','https://hive.example/\ud800','https://hive.example/a\\b','https://hive.example:65536','https://%FF/mcp','/mcp','https:','file:///mcp','data:hello'];
const server=url=>({name:'fixture',url,enabled:true,authentication:'none',startupTimeoutSeconds:1,toolTimeoutSeconds:3600});
const rows=[];
function add(servers,{enabled=true,hive='https://hive.example',credential='fixture-only',appId=null}={}){
  const document={schemaVersion:1,hostId:null,enabled,servers},hiveUrls=resourceUrls(hive);
  rows.push({document,hiveUrls,credential,appId,expected:capture(()=>renderMcpSections(document,new Set(hiveUrls),credential,appId))});
}
for(const url of urls)for(const authentication of ['none','agent-manager'])for(const enabled of [true,false])add([{...server(url),authentication}],{enabled});
for(const credential of ['fixture-only','quote"slash\\line\n','😀\ud800\udc00','\ud800','\udc00','\u2028\u2029',''])add([{...server('https://hive.example/mcp'),authentication:'agent-manager',enabled:false}],{credential,appId:'asdk_app_0123456789abcdef0123456789abcdef'});
for(const enabled of [true,false,null]){
  add([],{enabled});
  add([server('https://hive.example/mcp'),server('invalid')],{enabled});
  add([server('invalid'),server('https://hive.example/mcp')],{enabled});
  add([server('https://hive.example/mcp'),{...server('https://hive.example/mcp-dev'),name:'other',startupTimeoutSeconds:300,toolTimeoutSeconds:1}],{enabled,appId:'asdk_app_0123456789abcdef0123456789abcdef'});
}
for(const hive of ['http://localhost','http://127.0.0.1','http://[::1]','https://bücher.example'])for(const url of resourceUrls(hive))add([{...server(url),authentication:'agent-manager'}],{hive});
assert.equal(rows.length,143);assert(rows.some(r=>r.expected.error?.data.code==='access_denied'));
process.stdout.write(JSON.stringify({resources,rows}));
