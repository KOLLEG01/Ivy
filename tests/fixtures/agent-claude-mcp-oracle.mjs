import {mkdtemp,mkdir,writeFile,readFile,rm,lstat} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,relative} from 'node:path';
import assert from 'node:assert/strict';
import {ClaudeMcpConfigurationManager as Manager} from '../../dist/services/agent-manager/src/claude-mcp.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {digest,hashJson} from '../../dist/packages/contracts/src/canonical.js';

const hive='https://fixture.example',host='fixture',credential='fixture-only';
const server=(name='fixture',url=hive+'/mcp',authentication='agent-manager',enabled=true)=>({name,url,authentication,enabled,startupTimeoutSeconds:10,toolTimeoutSeconds:30});
const document=(servers=[server()],enabled=true)=>({schemaVersion:1,hostId:null,enabled,servers});
const definitions=[
  ['fresh'],['disabled'],['disabled-document-bad-url'],['disabled-server-bad-url'],['disabled-duplicate'],['duplicate'],
  ['numeric-prototype-names'],['empty-servers'],['empty-fragment'],['normalized'],['loopback-ipv4'],['loopback-ipv6'],['loopback-name'],
  ['http-remote'],['userinfo'],['fragment'],['wrong-authority-url'],['invalid-url'],['invalid-hive-disabled'],['unicode-credential'],['oversized-generated'],
  ['target-directory'],['state-directory'],['oversized-target'],['oversized-state'],['malformed-state'],['bom-state'],['null-state'],['primitive-state'],
  ['output-only'],['state-only'],['owned'],['wrong-authority'],['wrong-hash'],['wrong-version'],['partial-publication'],
].map(([mode])=>{
  let doc=document(),base=hive,secret=credential;
  switch(mode){
    case'disabled':doc.enabled=false;break;
    case'disabled-document-bad-url':doc=document([server('fixture','bad')],false);break;
    case'disabled-server-bad-url':doc=document([server('fixture','bad','agent-manager',false)]);break;
    case'disabled-duplicate':doc=document([server(),server('fixture','bad','agent-manager',false)]);break;
    case'duplicate':doc=document([server(),server()]);break;
    case'numeric-prototype-names':doc=document(['10','2','a','__proto__','constructor','__proto__'].map(name=>server(name)));break;
    case'empty-servers':doc.servers=[];break;
    case'empty-fragment':doc.servers[0].url=hive+'/mcp#';break;
    case'normalized':doc.servers[0].url='HTTPS://FIXTURE.EXAMPLE:443/x/../mcp';break;
    case'loopback-ipv4':doc=document([server('local','http://127.1/mcp','none')]);break;
    case'loopback-ipv6':doc=document([server('local','http://[::1]/mcp','none')]);break;
    case'loopback-name':doc=document([server('local','http://LOCALHOST/mcp','none')]);break;
    case'http-remote':doc=document([server('remote','http://remote.example/mcp','none')]);break;
    case'userinfo':doc.servers[0].url='https://user:password@fixture.example/mcp';break;
    case'fragment':doc.servers[0].url=hive+'/mcp#part';break;
    case'wrong-authority-url':doc.servers[0].url=hive+'/mcp?x=1';break;
    case'invalid-url':doc.servers[0].url='bad';break;
    case'invalid-hive-disabled':base='bad';doc.enabled=false;break;
    case'unicode-credential':secret='fixture-😀-\ud800-"';break;
    case'oversized-generated':secret='x'.repeat(4*1024*1024);break;
  }
  return{mode,document:doc,hive:base,credentialMode:mode==='oversized-generated'?'large':'normal',...(mode!=='oversized-generated'?{credential:secret}:{})};
});
const root=await mkdtemp(join(tmpdir(),'ivy-claude-mcp-oracle-'));
const snapshotFile=async path=>{try{if((await lstat(path)).isDirectory())return{kind:'directory'};const bytes=await readFile(path);return{hash:digest(bytes),bytes:bytes.length,text:bytes.length<=8192?bytes.toString('utf8'):null};}catch(error){return error.code==='ENOENT'?{missing:true}:{error:error.code};}};
const status=(manager,base)=>{assert.match(manager.status.observedAt,/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$/);const{observedAt,...value}=manager.status;return{...value,target:relative(base,value.target).replaceAll('\\','/')};};
const snapshot=async(manager,base)=>({status:status(manager,base),config:await snapshotFile(manager.target),state:await snapshotFile(manager.target+'.state.json')});
const client=(calls,fail=false)=>({async request(method,params){calls.push({method,params});if(fail)throw new IvyError('service_unavailable','fixture unavailable','unknown');assert.equal(method,'objects.stat');throw new IvyError('not_found','fixture missing');}});
const owned=()=>Buffer.from('{"prior":true}\n');
async function install(base,mode){
  await mkdir(base);const target=join(base,'claude-mcp.json'),state=target+'.state.json';
  if(mode==='target-directory')await mkdir(target);
  if(mode==='state-directory')await mkdir(state);
  if(mode==='oversized-target')await writeFile(target,Buffer.alloc(4*1024*1024+1,32));
  if(mode==='oversized-state')await writeFile(state,Buffer.alloc(4097,32));
  if(mode==='malformed-state')await writeFile(state,'{');
  if(mode==='bom-state')await writeFile(state,'\ufeffnull');
  if(mode==='null-state')await writeFile(state,'null');
  if(mode==='primitive-state')await writeFile(state,'false');
  if(['output-only','owned','wrong-authority','wrong-hash','wrong-version','partial-publication'].includes(mode))await writeFile(target,owned());
  if(['state-only','owned','wrong-authority','wrong-hash','wrong-version','partial-publication'].includes(mode))await writeFile(state,JSON.stringify({schemaVersion:mode==='wrong-version'?2:1,authority:mode==='wrong-authority'?'other':hashJson({hostId:host,hiveUrl:hive}),appliedHash:mode==='wrong-hash'||mode==='partial-publication'?'sha256:'+'0'.repeat(64):digest(owned())}));
  return target;
}
try{
  const rows=[];
  for(const definition of definitions){
    const base=join(root,definition.mode),target=await install(base,definition.mode),calls=[];
    const secret=definition.credentialMode==='large'?'x'.repeat(4*1024*1024):definition.credential;
    const manager=new Manager(target,host,definition.hive,secret,definition.document);
    await manager.synchronize(client(calls));await manager.close();
    rows.push({...definition,calls,expected:await snapshot(manager,base)});
  }
  const base=join(root,'lifetime'),target=await install(base,'fresh'),calls=[];
  const manager=new Manager(target,host,hive,credential,document());
  const history=[];
  await manager.synchronize(client(calls));history.push(await snapshot(manager,base));
  await manager.synchronize(client(calls,true));history.push(await snapshot(manager,base));
  const changed=Buffer.from('{"external":true}\n');await writeFile(target,changed);
  await writeFile(target+'.state.json',JSON.stringify({schemaVersion:1,authority:hashJson({hostId:host,hiveUrl:hive}),appliedHash:digest(changed)}));
  await manager.synchronize(client(calls));history.push(await snapshot(manager,base));
  const bootstrapBase=join(root,'bootstrap'),bootstrapTarget=await install(bootstrapBase,'fresh');
  const first=new Manager(bootstrapTarget,host,hive,credential,document([],false));await first.bootstrap();
  const next=new Manager(bootstrapTarget,host,hive,credential,document());
  await next.bootstrap();const afterBootstrap=await snapshot(next,bootstrapBase);
  await next.synchronize(client([],true));const afterUnavailable=await snapshot(next,bootstrapBase);
  const coalescedBase=join(root,'coalesced'),coalescedTarget=await install(coalescedBase,'fresh');
  const coalesced=new Manager(coalescedTarget,host,hive,credential,document());
  let release,entered;const held=new Promise(resolve=>{release=resolve;}),started=new Promise(resolve=>{entered=resolve;});
  const firstCalls=[],secondCalls=[];
  const blocked={async request(method,params){firstCalls.push({method,params});entered();await held;throw new IvyError('not_found','fixture missing');}};
  const run=coalesced.synchronize(blocked);await started;
  const joined=coalesced.synchronize(client(secondCalls));let closed=false;const close=coalesced.close().then(()=>{closed=true;});
  await Promise.resolve();const closedWhileBlocked=closed;release();await Promise.all([run,joined,close]);
  let relativeError=null;try{new Manager('relative',host,hive,credential,document());}catch(error){relativeError=IvyError.from(error).code;}
  process.stdout.write(JSON.stringify({rows,lifetime:{calls,history},bootstrap:{afterBootstrap,afterUnavailable},coalesced:{firstCalls,secondCalls,closedWhileBlocked,expected:await snapshot(coalesced,coalescedBase)},relativeError}));
}finally{await rm(root,{recursive:true,force:true});}
