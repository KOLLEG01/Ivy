import {mkdtemp,mkdir,writeFile,readFile,rm,symlink} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,relative} from 'node:path';
import assert from 'node:assert/strict';
import {McpConfigurationManager} from '../../dist/services/agent-manager/src/environment.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {digest} from '../../dist/packages/contracts/src/canonical.js';
const defaults=enabled=>({schemaVersion:1,hostId:null,enabled,servers:[]});
const manager=(home,enabled=true)=>new McpConfigurationManager(home,'fixture','https://fixture.example','fixture-only',defaults(enabled));
const status=(m,root)=>{assert(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/.test(m.status.observedAt));const {observedAt,...value}=m.status;return {...value,target:relative(root,value.target).replaceAll('\\','/')};};
const client=(revision=0)=>({async request(method){if(method==='objects.stat')throw new IvyError('not_found','missing');assert.equal(method,'openai.getConfiguration');return {revision,configuration:{schemaVersion:1,appId:null}};}});
const root=await mkdtemp(join(tmpdir(),'ivy-mcp-manager-'));
const bootstraps=[];
try{
  for(const setup of ['fresh','disabled','empty-state','invalid-state','null-state','state-directory','oversized-state','missing-home']){
    const base=join(root,setup),home=join(base,'home');await mkdir(base);if(setup!=='missing-home')await mkdir(home);
    if(setup.includes('state')){await mkdir(join(home,'.ivy-agent-mcp'));const path=join(home,'.ivy-agent-mcp/state.json');if(setup==='state-directory')await mkdir(path);else await writeFile(path,setup==='empty-state'?'':setup==='invalid-state'?'{':setup==='null-state'?'null':Buffer.alloc(4*1024*1024+1,32));}
    const m=manager(home,setup!=='disabled'),initial=status(m,base);let error=null;
    try{await m.bootstrap();}catch(e){error=IvyError.from(e).code;}
    await m.close();
    const config=await readFile(join(home,'config.toml')).then(digest,e=>{if(e.code==='ENOENT')return null;throw e;});
    bootstraps.push({setup,initial,error,status:status(m,base),config});
  }
  const base=join(root,'lifecycle'),home=join(base,'home');await mkdir(base);await mkdir(home);
  const m=manager(home),lifecycle=[status(m,base)];
  await m.synchronize(client());lifecycle.push(status(m,base));
  await m.synchronize({async request(){throw new IvyError('service_unavailable','fixture failure','unknown');}});lifecycle.push(status(m,base));
  const good=await readFile(join(home,'config.toml'));await writeFile(join(home,'config.toml'),good.toString().replace('approval_policy = "never"','approval_policy = "ask"'));
  await m.synchronize(client(1));lifecycle.push(status(m,base));
  await writeFile(join(home,'config.toml'),good);await m.synchronize(client(2));await m.close();lifecycle.push(status(m,base));
  let release,entered;const gate=new Promise(r=>release=r),started=new Promise(r=>entered=r),calls=[];
  const held={async request(method,params){calls.push(method);if(calls.length===1){entered();await gate;}return client(3).request(method,params);}};
  const first=m.synchronize(held);await started;let secondCalls=0,closed=false;
  const second=m.synchronize({async request(){secondCalls++;throw Error('unused');}}),close=m.close().then(()=>closed=true);
  assert.equal(first,second);await Promise.resolve();assert.equal(closed,false);
  const during=status(m,base);release();await Promise.all([first,second,close]);assert.equal(secondCalls,0);
  const coalescing={during,after:status(m,base),calls,secondCalls,closed};
  m.synchronize(client(4));await m.close();const detached=status(m,base);
  const linkBase=join(root,'linked'),real=join(linkBase,'home'),link=join(linkBase,'link');await mkdir(linkBase);await mkdir(real);await symlink(real,link,process.platform==='win32'?'junction':'dir');
  const linked=manager(link),linkInitial=status(linked,linkBase);await linked.synchronize(client());await linked.close();
  process.stdout.write(JSON.stringify({bootstraps,lifecycle,coalescing,detached,linked:{initial:linkInitial,after:status(linked,linkBase)}}));
}finally{await rm(root,{recursive:true,force:true});}
