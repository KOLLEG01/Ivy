import {mkdtemp,mkdir,readFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import assert from 'node:assert/strict';
import {McpConfigurationManager,environmentName} from '../../dist/services/agent-manager/src/environment.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {hashJson} from '../../dist/packages/contracts/src/canonical.js';
const hostId='fixture',hive='https://fixture.example',credential='fixture-only';
const document=(scope,enabled,name='fixture')=>({schemaVersion:1,hostId:scope,enabled,servers:[{name,url:hive+'/mcp',enabled:true,authentication:'agent-manager',startupTimeoutSeconds:15,toolTimeoutSeconds:45}]});
const snapshot=(revision=0,appId=null)=>({revision,configuration:{schemaVersion:1,appId}});
const cases=[];
for(const enabled of [true,false])for(const global of ['missing',null,false,true])for(const host of ['missing',null,false,true])cases.push({enabled,global,host});
for(const mode of ['global-stat-fail','global-read-fail','host-stat-fail','host-read-fail','openai-fail','openai-invalid','openai-changed','bad-authority-url','bad-server-url','external-credential','duplicate','global-scope','host-scope','global-contract','host-version','host-schema','archived-global','archived-host'])cases.push({enabled:true,global:true,host:true,mode});
cases.push({enabled:false,global:'missing',host:'missing',mode:'openai-fail'});
cases.push({enabled:false,global:'missing',host:'missing',mode:'bad-authority-url'});
const root=await mkdtemp(join(tmpdir(),'ivy-mcp-prepare-')),rows=[];
try {
  for(const [index,test] of cases.entries()){
    const home=join(root,String(index));await mkdir(home);
    const defaults=document(null,test.enabled),global=test.global==='missing'?null:document(null,test.global,'global'),host=test.host==='missing'?null:document(hostId,test.host,'host');
    const effectiveHive=test.mode==='bad-authority-url'?'invalid':hive;
    const chosen=host??global??defaults;
    if(test.mode==='bad-server-url')chosen.servers[0].url='invalid';
    if(test.mode==='external-credential')chosen.servers[0].url='https://external.example/mcp';
    if(test.mode==='duplicate')chosen.servers.push({...chosen.servers[0]});
    if(test.mode==='global-scope')global.hostId=hostId;
    if(test.mode==='host-scope')host.hostId=null;
    if(test.mode==='host-schema')host.servers[0].enabled='true';
    const replies=[];
    const connection={async request(method,params){
      let result,error;
      const scope=method==='objects.stat'?(params.path==='/'+environmentName('mcp',null)?'global':'host'):params.objectId;
      if(test.mode===scope+'-'+(method==='objects.stat'?'stat':'read')+'-fail'||method==='openai.getConfiguration'&&test.mode==='openai-fail')error={code:'service_unavailable',outcome:'unknown'};
      else if(method==='objects.stat'){
        const doc=scope==='global'?global:host;
        if(!doc)error={code:'not_found',outcome:'not_executed'};
        else result={id:scope,currentRevision:scope==='global'?7:11,effectivelyArchived:test.mode==='archived-'+scope};
      }else if(method==='objects.read'){
        const doc=scope==='global'?global:host;
        result={object:{id:scope,contractKey:test.mode==='global-contract'&&scope==='global'?'other':'agent/mcp-configuration'},revision:{revision:99,contractVersion:test.mode==='host-version'&&scope==='host'?'2.0.0':'1.0.0',contentHash:hashJson(doc)},content:{encoding:'json',value:doc}};
      }else{assert.equal(method,'openai.getConfiguration');result=test.mode==='openai-invalid'?{revision:0,configuration:{schemaVersion:1,appId:'wrong'}}:test.mode==='openai-changed'?snapshot(23,'asdk_app_0123456789abcdef0123456789abcdef'):snapshot();}
      replies.push({method,params,...(error?{error}:{result})});
      if(error)throw new IvyError(error.code,'fixture failure',error.outcome);return result;
    }};
    const manager=new McpConfigurationManager(home,hostId,effectiveHive,credential,defaults);
    await manager.synchronize(connection);await manager.close();
    let expected;
    if(manager.status.code!==null){
      assert.equal(await readFile(join(home,'config.toml')).then(()=>true,e=>{if(e.code==='ENOENT')return false;throw e;}),false);
      expected={error:{code:manager.status.code,state:manager.status.state}};
    }else{
      const text=(await readFile(join(home,'config.toml'))).toString();
      const state=JSON.parse((await readFile(join(home,'.ivy-agent-mcp/state.json'))).toString());
      expected={result:{text,state,applied:manager.status.state==='applied',managedBytes:manager.status.bytes,desiredVersion:manager.status.desiredVersion}};
    }
    rows.push({mode:test.mode??'inheritance',defaults,hostId,hive:effectiveHive,credential,replies,expected});
  }
}finally{await rm(root,{recursive:true,force:true});}
assert.equal(rows.length,52);assert(rows.some(r=>r.expected.error)&&rows.some(r=>r.expected.result));
process.stdout.write(JSON.stringify(rows));
