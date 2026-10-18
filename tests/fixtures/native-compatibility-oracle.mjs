import assert from 'node:assert/strict';
import {mkdir,writeFile,readFile} from 'node:fs/promises';
import {join} from 'node:path';
import {verifyConfiguredNativeCatalog,verifyNativeCatalogCompatibility} from '../../dist/packages/host-runtime/src/native-catalog-compatibility.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {hashJson} from '../../dist/packages/contracts/src/canonical.js';
let text='';for await(const bytes of process.stdin)text+=bytes;
const root=JSON.parse(text).root,rows=[];
const hash='sha256:'+'f'.repeat(64);
const sources=['codex-0.142.3/catalog.json',...['0.154.0','0.158.0','0.159.2'].flatMap(v=>[`codex-${v}/catalog.json`,`codex-${v}/catalog.linux-x64.json`])];
const catalogs=await Promise.all(sources.map(async file=>JSON.parse(await readFile(join('specs/native',file),'utf8'))));
// The structural Catalog contract, not NativeContract construction, is the original boundary.
const small=catalogs.map(c=>({...c,clientRequests:[],serverRequests:[],serverNotifications:[],clientNotifications:[],derivedLegacyTypes:[]}));
const status=c=>({serviceNodeId:'native-agent',hostId:'fixture',nativeVersion:c.version,nativeExecutableHash:c.nativeExecutableHash,catalogHash:hashJson(c),epoch:'epoch',pid:42,state:'ready',observedAt:'2026-01-02T03:04:05.006Z',code:null,initialized:{},pendingInputs:0,operations:{retained:0,maximum:100,bytes:0,maximumBytes:1048576},observedMethods:[]});
const node={serviceNodeId:'native-agent',hostId:'fixture',connected:true,synced:true,ready:true,desiredEnabled:true,stale:false};
async function collect(label,edit=()=>{}){
 const input={root:join(root,String(rows.length)),component:'fixture',instance:'fixture-instance',catalogs:small.map((c,i)=>({file:sources[i],value:c})),pages:[{items:[node],nextCursor:null}],status:status(small[6]),mode:'compatibility'};
 edit(input);const native=join(input.root,'dist/specs/native');await mkdir(native,{recursive:true});
 for(const c of input.catalogs){const file=join(native,c.file);await mkdir(join(file,'..'),{recursive:true});await writeFile(file,JSON.stringify(c.value));}
 let page=0;const trace=[];
 const client={async request(method,params,options){trace.push({method,params,...(method==='tools.list'?{}:{timeout:options.timeoutMs})});
  if(input.fail===method)throw new IvyError('fixture_failure','Read failed.','unknown');
  if(method==='serviceNodes.list')return structuredClone(input.pages[page++]);
  if(method==='tools.list')return {provider:{node:{serviceNodeId:params.serviceNodeId}},items:[{qualifiedName:'agent.status',definition:{namespace:'agent',name:'status',interfaceVersion:input.interface??'1.0.0',annotations:{readOnlyHint:true},inputSchema:{},outputSchema:{}},definitionHash:'sha256:'+'a'.repeat(64),catalogHash:'sha256:'+'b'.repeat(64)}],nextCursor:null};
  if(method==='tools.call')return structuredClone(input.status);
  throw Error('Unexpected request '+method);
 }};
 let expected;try{if(input.mode==='configured')await verifyConfiguredNativeCatalog(input.root,input.instance,input.settings);else await verifyNativeCatalogCompatibility(client,input.root,input.component);expected={ok:true};}catch(e){expected={ok:false,error:IvyError.from(e).toWire()};}
 rows.push({label,input,expected,trace});
}
for(let i=0;i<small.length;i++)await collect('retained-'+i,x=>x.status=status(small[i]));
await collect('empty',x=>x.catalogs=[]);
await collect('nonmatching-files',x=>x.catalogs=[{file:'codex-preview/catalog.json',value:{}},{file:'codex-1.2.3/catalog.WIN32-x64.json',value:{}}]);
await collect('invalid-catalog',x=>x.catalogs[0].value={});
for(const field of ['connected','synced','ready','desiredEnabled','stale'])await collect('filter-'+field,x=>{x.pages[0].items=[{...node,[field]:field==='stale'}];x.status={};});
for(const [field,value] of [['hostId','changed'],['serviceNodeId','changed'],['state','stopped'],['nativeVersion','0.154.0'],['nativeExecutableHash',hash],['catalogHash',hash],['epoch',null]])await collect('identity-'+field,x=>x.status[field]=value);
await collect('null-status',x=>x.status=null);
await collect('no-online-owners',x=>x.pages=[{items:[],nextCursor:null}]);
await collect('utf16-component',x=>{x.component='fixture\ud800';x.status.catalogHash=hash;});
await collect('interface',x=>x.interface='2.0.0');
for(const method of ['serviceNodes.list','tools.list','tools.call'])await collect('failure-'+method,x=>x.fail=method);
await collect('pagination',x=>x.pages=[{items:[],nextCursor:'next'},{items:[node],nextCursor:null}]);
await collect('cursor-loop',x=>x.pages=[{items:[],nextCursor:'next'},{items:[],nextCursor:'next'}]);
await collect('fresh-bindings',x=>x.pages=[{items:[node],nextCursor:'next'},{items:[node],nextCursor:null}]);
for(const [version,executable] of [[small[6].version,small[5].nativeExecutableHash],[small[6].version,hash],['0.1.0',hash]])await collect('configured',x=>{x.mode='configured';x.settings={nativeVersion:version,nativeExecutableHash:executable};});
await collect('configured-utf16',x=>{x.mode='configured';x.instance='fixture\ud800';x.settings={nativeVersion:small[0].version,nativeExecutableHash:hash};});
assert.equal(rows[0].expected.ok,true);
for(const code of ['incompatible_native_catalog','compatibility_unknown','fixture_failure','contract_version_conflict','native_version_unsupported'])assert(rows.some(r=>r.expected.error?.data.code===code),code);
process.stdout.write(JSON.stringify(rows));
