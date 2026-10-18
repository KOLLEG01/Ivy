import {registerHooks} from 'node:module';
import {readFileSync} from 'node:fs';
import {mkdtemp,mkdir,writeFile,readFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import assert from 'node:assert/strict';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {digest,hashJson} from '../../dist/packages/contracts/src/canonical.js';
const hook=registerHooks({load(url,context,next){
  if(url.endsWith('/dist/services/agent-manager/src/environment.js'))return {format:'module',source:readFileSync(new URL(url),'utf8')+'\nexport {renderMcpSections,renderCodex,beginMcp,endMcp,beginPermissions,endPermissions};\n',shortCircuit:true};
  return next(url,context);
}});
const original=await import('../../dist/services/agent-manager/src/environment.js');hook.deregister();
const hive='https://fixture.example',hostId='fixture',credential='fixture-only';
const authority=hashJson({hive,hostId});
const document=enabled=>({schemaVersion:1,hostId:null,enabled,servers:[{name:'fixture',url:hive+'/mcp',authentication:'agent-manager',enabled:true,startupTimeoutSeconds:10,toolTimeoutSeconds:30}]});
const sections=original.renderMcpSections(document(true),new Set([hive+'/mcp']),credential,null);
const current=original.renderCodex(sections),oldCurrent=original.renderCodex([]);
const legacyMcp=[original.beginMcp,...sections,original.endMcp].join('\n');
const legacyPermissions=[original.beginPermissions,'approval_policy = "never"','default_permissions = ":danger-full-access"',original.endPermissions].join('\n');
const state=(schemaVersion,appliedBlockHash,extra={})=>({schemaVersion,authority,appliedBlockHash,appliedVersion:null,...extra});
const cases=[];
for(const enabled of [true,false]) {
  for(const source of ['', '\ufeff# user\r\n[project]\nx=1\r\n',current,oldCurrent,legacyMcp+'\n'+legacyPermissions,current+'\n'+legacyMcp,
    'approval_policy = "never"\ndefault_permissions = ":danger-full-access"\n','approval_policy = "ask"\n',
    '[mcp_servers.fixture]\nx=1','[mcp_servers.Fixture]\nx=1','sandbox_mode = "danger-full-access"','[sandbox_workspace_write]\nx=1'])
    cases.push({enabled,source});
  for(const stored of [null,false,0,'',{},[],true,1,'bad',state(3,null),state(2,null,{authority:'other'}),state(1,null),state(2,null),state(2,'bad')])cases.push({enabled,source:'',stored});
  for(const version of [1,2])for(const source of ['',current,oldCurrent,legacyMcp,legacyPermissions,legacyMcp+'\n'+legacyPermissions])
    for(const previous of [null,'bad',digest(current),digest(oldCurrent),digest(legacyMcp)])cases.push({enabled,source,stored:state(version,previous,{appliedPermissionsBlockHash:digest(legacyPermissions)})});
  for(const schemaVersion of [1,2])for(const source of ['',current,legacyMcp])cases.push({enabled,source,stored:{schemaVersion,authority}});
  cases.push({enabled,source:current.replace('\n[mcp_servers.', '\nmodel = "fixture-model"\n[mcp_servers.'),stored:state(2,digest(current))});
  cases.push({enabled,source:current.replace(/# <<<[^]*$/,''),stored:state(2,digest(current))});
  cases.push({enabled,source:'[apps.asdk_app_0123456789abcdef0123456789abcdef]\nenabled=true',appId:'asdk_app_0123456789abcdef0123456789abcdef'});
  cases.push({enabled,source:'',appId:'asdk_app_0123456789abcdef0123456789abcdef'});
}
const root=await mkdtemp(join(tmpdir(),'ivy-mcp-output-'));
const rows=[];
try {
  for(const [index,test] of cases.entries()) {
    const home=join(root,String(index));await mkdir(home);
    await writeFile(join(home,'config.toml'),test.source);
    if(Object.hasOwn(test,'stored')) {await mkdir(join(home,'.ivy-agent-mcp'));await writeFile(join(home,'.ivy-agent-mcp/state.json'),JSON.stringify(test.stored));}
    const openai={revision:0,configuration:{schemaVersion:1,appId:test.appId??null}};
    const defaults=document(test.enabled);
    const rendered=original.renderMcpSections(defaults,new Set([hive+'/mcp']),credential,openai.configuration.appId);
    const desiredVersion=hashJson({document:original.selectEnvironmentDocument(defaults,null,null).version,approvalPolicy:'never',defaultPermissions:':danger-full-access',openai});
    const manager=new original.McpConfigurationManager(home,hostId,hive,credential,defaults);
    await manager.synchronize({async request(method){if(method==='objects.stat')throw new IvyError('not_found','fixture missing');assert.equal(method,'openai.getConfiguration');return openai;}});
    await manager.close();
    const bytes=await readFile(join(home,'config.toml'));
    const after=await readFile(join(home,'.ivy-agent-mcp/state.json')).then(b=>JSON.parse(b.toString()),e=>{if(e.code==='ENOENT')return undefined;throw e;});
    let expected;
    if(manager.status.code!==null){
      assert.equal(manager.status.code,'target_conflict',JSON.stringify({index,status:manager.status}));
      assert.equal(bytes.toString(),test.source);assert.deepEqual(after,test.stored);
      expected={error:manager.status.code};
    }else{
      assert.equal(manager.status.appliedVersion,desiredVersion);
      expected={result:{text:bytes.toString(),state:after,applied:manager.status.state==='applied',managedBytes:manager.status.bytes}};
    }
    rows.push({source:new TextDecoder('utf-8',{fatal:true}).decode(Buffer.from(test.source)),state:test.stored??null,authority,sections:rendered,serverNames:defaults.servers.map(s=>s.name),appId:openai.configuration.appId,desiredVersion,expected});
  }
} finally {await rm(root,{recursive:true,force:true});}
assert(rows.some(row=>row.expected.result)&&rows.some(row=>row.expected.error));
process.stdout.write(JSON.stringify(rows));
