import {registerHooks} from 'node:module';
import {existsSync,readFileSync} from 'node:fs';
import {mkdir,writeFile,readFile,rm} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
registerHooks({resolve(s,c,n){if(s.startsWith('.')&&s.endsWith('.js')&&c.parentURL){const u=new URL(s.slice(0,-3)+'.ts',c.parentURL);if(existsSync(fileURLToPath(u)))return n(u.href,c);}return n(s,c);}});
const {instanceConfig}=await import('../../packages/host-runtime/src/config.ts');
const {HealthFile}=await import('../../packages/host-runtime/src/health.ts');
const {validateTaskBoard}=await import('../../packages/contracts/src/task-board-validation.ts');
const {hashJson}=await import('../../packages/contracts/src/canonical.ts');
const {IvyError,requireThat}=await import('../../packages/contracts/src/errors.ts');
const {ServiceClient}=await import('../../packages/sdk/src/service.ts');
const source=readFileSync('services/task-board/src/main.ts','utf8');
const first=source.slice(source.indexOf('  const config = await instanceConfig(path);'),source.indexOf('  const settings = config.settings'));
assert.ok(first.includes("config.componentId === 'task-board' && config.credential"));
let phase='host',settingsCalls=0;
const admit=new Function('instanceConfig','validateTaskBoard','requireThat','return async function(path){'+first+'return config;}')(
 instanceConfig,(...args)=>{phase='settings';settingsCalls++;return validateTaskBoard(...args);},(...args)=>{phase='identity';return requireThat(...args);});
const construct=config=>{
 phase='transport';let credentials=0;
 const unopened=new ServiceClient({publicBaseUrl:config.publicBaseUrl,credential:()=>{credentials++;return config.credential;},
  identity:{serviceNodeId:config.serviceNodeId,serviceName:'task-board',instanceMode:'singleton',hostId:config.hostId,version:config.version,buildId:config.buildId,hiveProtocol:1},
  registry:()=>{throw Error('Unopened constructor must not discover a registry');},handlers:{},reconcile:async()=>{throw Error('Unopened constructor must not reconcile');}});
 assert.equal(unopened.ready,false);assert.equal(credentials,0);return config;
};
const root=resolve(process.argv[2]),build={buildId:'sha256:'+'a'.repeat(64),version:'1.0.0'},rows=[];
const effects=c=>({data:existsSync(c.dataRoot),work:existsSync(c.workRoot),logs:existsSync(c.logsRoot),health:existsSync(join(c.dataRoot,'health.json'))});
const outcome=async work=>{try{return {ok:await work()};}catch(e){return {error:IvyError.from(e).code};}};
for(const name of ['valid','wrong component','missing credential','invalid settings','build mismatch','overlapping roots','logs inside artifacts','missing build','UTF16 owner',
 'invalid settings before UTF16 URL','identity before UTF16 URL','missing credential before UTF16 URL','invalid settings before UTF16 credential','identity before UTF16 credential',
 'UTF16 URL path','UTF16 URL query','UTF16 URL host','invalid URL','UTF16 URL only','remote plain HTTP','URL credentials','UTF16 credential','UTF16 credential low','Latin1 credential','BMP credential','CRLF credential','UTF16 service identity','URL TAB newline']){
 const base=join(root,'case-'+rows.length),artifactRoot=join(base,'artifact');
 const config={schemaVersion:1,hostId:'fixture-host',instanceId:'fixture-instance',serviceNodeId:'fixture-task-board',componentId:'task-board',publicBaseUrl:'http://127.0.0.1:1/',dataRoot:join(base,'data'),workRoot:join(base,'work'),logsRoot:join(base,'logs'),artifactRoot,...build,credential:'fixture-token',settings:{principalId:'fixture-principal',rootObjectId:null,scheduler:{enabled:false,intervalMs:1000,pageSize:50},phoneTarget:null}};
 if(name==='wrong component')config.componentId='other';
 if(name==='missing credential')delete config.credential;
 if(name==='invalid settings')config.settings.scheduler.intervalMs=0;
 if(name==='overlapping roots')config.dataRoot=join(artifactRoot,'data');
 if(name==='logs inside artifacts')config.logsRoot=join(artifactRoot,'logs');
 if(name==='UTF16 owner')config.settings.principalId='owner\ud800';
 if(name==='invalid settings before UTF16 URL'){config.settings.scheduler.intervalMs=0;config.publicBaseUrl='\ud800';}
 if(name==='identity before UTF16 URL'){config.componentId='other';config.publicBaseUrl='\ud800';}
 if(name==='missing credential before UTF16 URL'){delete config.credential;config.publicBaseUrl='\ud800';}
 if(name==='invalid settings before UTF16 credential'){config.settings.scheduler.intervalMs=0;config.credential='token\ud800';}
 if(name==='identity before UTF16 credential'){config.componentId='other';config.credential='token\ud800';}
 if(name==='UTF16 URL path')config.publicBaseUrl+='\ud800';
 if(name==='UTF16 URL query')config.publicBaseUrl+='?x=\ud800';
 if(name==='UTF16 URL host')config.publicBaseUrl='https://\ud800.invalid/';
 if(name==='invalid URL')config.publicBaseUrl='not-a-url';
 if(name==='UTF16 URL only')config.publicBaseUrl='\ud800';
 if(name==='remote plain HTTP')config.publicBaseUrl='http://example.invalid/';
 if(name==='URL credentials')config.publicBaseUrl='http://u:p@localhost/';
 if(name==='UTF16 credential')config.credential='token\ud800';
 if(name==='UTF16 credential low')config.credential='token\udc00';
 if(name==='Latin1 credential')config.credential='token\u00e9';
 if(name==='BMP credential')config.credential='token\u20ac';
 if(name==='CRLF credential')config.credential='token\r\n';
 if(name==='UTF16 service identity')config.serviceNodeId='service\ud800';
 if(name==='URL TAB newline')config.publicBaseUrl='http:\n//127.0.0.1:1/\tpath';
 const savedBuild=name==='missing build'?null:{...build,...name==='build mismatch'?{version:'other'}:{}};
 const path=join(base,'instance.json');await mkdir(join(artifactRoot,'dist'),{recursive:true});
 if(savedBuild)await writeFile(join(artifactRoot,'dist/build-info.json'),JSON.stringify(savedBuild));
 await writeFile(path,JSON.stringify(config));
 phase='host';settingsCalls=0;let result;
 try{result={ok:hashJson(construct(await admit(path)))};}catch(error){const e=IvyError.from(error);result={error:e.code,phase,message:phase==='settings'?'$SETTINGS':e.message,outcome:e.outcome};}
 rows.push({name,path,config,build:savedBuild,result,settingsCalls,effects:effects(config)});
 await rm(base,{recursive:true,force:true});
}
const config=structuredClone(rows[0].config);config.dataRoot=join(root,'health-data');
const health=new HealthFile(config),healthRows=[];
await health.write(true,'x'.repeat(4095)+'\ud83d\ude00',7);
const written=JSON.parse(await readFile(join(config.dataRoot,'health.json'),'utf8'));
assert.equal(written.instanceId,config.instanceId);assert.equal(written.componentId,'task-board');assert.equal(written.buildId,build.buildId);assert.equal(written.pid,process.pid);assert.ok(written.bootId);assert.ok(written.startedAt);assert.ok(written.observedAt);assert.equal(written.launchId,'fixture-launch');
const bootId=written.bootId;for(const key of ['pid','bootId','startedAt','observedAt'])delete written[key];
healthRows.push({name:'missing control',result:await outcome(()=>health.control())});
const control={schemaVersion:1,instanceId:config.instanceId,launchId:'fixture-launch',bootId,action:'drain',requestedAt:'2026-01-02T03:04:05.006Z'};
for(const [name,change]of [['foreign instance',{instanceId:'other'}],['foreign boot',{bootId:'other'}],['foreign launch',{launchId:'other'}],['exact drain',{}],['exact shutdown',{action:'shutdown'}],['invalid control',{action:'other'}]]){
 const value={...control,...change};await writeFile(join(config.dataRoot,'control.json'),JSON.stringify(value));
 const result=await outcome(()=>health.control());if(result.ok?.bootId===bootId)result.ok.bootId='$BOOT';
 healthRows.push({name,value:{...value,bootId:value.bootId===bootId?'$BOOT':value.bootId},result});
}
await rm(config.dataRoot,{recursive:true,force:true});
process.stdout.write(JSON.stringify({rows,health:{config,written,rows:healthRows}}));
