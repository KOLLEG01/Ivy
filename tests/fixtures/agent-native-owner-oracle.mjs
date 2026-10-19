import {readFileSync,mkdirSync,writeFileSync,existsSync,realpathSync,symlinkSync} from 'node:fs';
import {readFile,realpath} from 'node:fs/promises';
import {join,dirname,relative} from 'node:path';
import {createHash} from 'node:crypto';
import assert from 'node:assert/strict';
import {startNativeProcess} from '../../dist/services/agent-manager/src/process.js';
import {IvyError,requireThat} from '../../dist/packages/contracts/src/errors.js';
import {initializedServerVersion} from '../../dist/packages/host-runtime/src/codex-daemon.js';
const digest=bytes=>'sha256:'+createHash('sha256').update(bytes).digest('hex');
let bytes='';for await(const chunk of process.stdin)bytes+=chunk;const input=JSON.parse(bytes);
const limits={maxOperations:100,maxJournalBytes:1048576,maxPendingInputs:8,maxNotificationBytes:1048576};
const paid=['ANTHROPIC_API_KEY','ANTHROPIC_AUTH_TOKEN','ANTHROPIC_BASE_URL','CLAUDE_CODE_USE_BEDROCK','CLAUDE_CODE_USE_VERTEX','CLAUDE_CODE_USE_FOUNDRY'];
// Only the fixture subprocess environment changes. No actual process is launched.
for(const key of paid)delete process.env[key];
const save=(path,value)=>{mkdirSync(dirname(path),{recursive:true});writeFileSync(path,JSON.stringify(value));};
function prepare(root,variant){
 mkdirSync(root,{recursive:true});
 const artifactRoot=join(root,'artifact'),dataRoot=join(root,'data'),bin=join(root,'bin');
 mkdirSync(bin);mkdirSync(dataRoot);mkdirSync(artifactRoot);
 const executable=join(bin,variant==='wrapper'?'native.CMD':'native.exe');writeFileSync(executable,'pinned synthetic file; never execute\n');
 const home=join(dataRoot,'native');
 const version=variant.startsWith('version-')?variant.slice(8):'0.142.3';
 const settings={nativeExecutable:executable,nativeVersion:version,nativeExecutableHash:digest(readFileSync(executable)),codexHome:home,appServer:{mode:'owned-stdio'},limits};
 if(variant==='external')settings.appServer={mode:'external-proxy',socketPath:join(root,'external.sock'),expectedCodexHome:join(root,'server-home')};
 if(variant==='default')delete settings.appServer;
 if(variant==='relative-socket')settings.appServer={mode:'external-proxy',socketPath:'relative.sock'};
 if(variant==='relative-server-home')settings.appServer={mode:'external-proxy',expectedCodexHome:'relative'};
 if(variant==='relative-executable')settings.nativeExecutable='relative.exe';
 if(variant==='overlap')settings.codexHome=artifactRoot;
 if(variant==='data-home')settings.codexHome=dataRoot;
 if(variant==='home-link'){mkdirSync(join(root,'outside'));symlinkSync(join(root,'outside'),home,'junction');}
 if(variant.startsWith('claude-')){
  const adapter=join(root,'adapter'),source=join(adapter,'dist','src');mkdirSync(source,{recursive:true});
  writeFileSync(join(adapter,'package-lock.json'),'{}\n');writeFileSync(join(source,'adapter.mjs'),'// pinned fixture\n');writeFileSync(join(source,'extra.mjs'),'// extra\n');
  const manifest=['dist/src/adapter.mjs','dist/src/extra.mjs','package-lock.json'].map(name=>name+'\0'+digest(readFileSync(join(adapter,name))).slice(7)+'\0').join('');
  settings.nativeExecutableHash=digest(manifest);
  const cli=join(bin,'claude.cmd');writeFileSync(cli,'pinned CLI bytes; never execute\n');
  settings.appServer={mode:'claude-adapter',adapterRoot:adapter,nodeExecutableHash:digest(readFileSync(executable)),claudeExecutable:cli,claudeExecutableHash:digest(readFileSync(cli)),defaultModel:'fixture-model'};
  if(variant==='claude-not-owned')settings.codexHome=join(root,'shared-home');
  if(variant==='claude-patcher-off')settings.patcherEnabled=false;
  if(variant==='claude-explicit-skills')settings.skillsRoot=join(dataRoot,'skills');
  if(variant==='claude-adapter-hash')settings.nativeExecutableHash='sha256:'+'0'.repeat(64);
  if(variant==='claude-cli-hash')settings.appServer.claudeExecutableHash='sha256:'+'0'.repeat(64);
  if(variant==='claude-node-hash')settings.appServer.nodeExecutableHash='sha256:'+'0'.repeat(64);
  const config=join(dataRoot,'claude-config');
  if(variant==='claude-billing-key')save(join(config,'settings.json'),{env:{ANTHROPIC_API_KEY:null}});
  if(variant==='claude-billing-helper')save(join(config,'settings.json'),{apiKeyHelper:null});
  if(variant==='claude-billing-console')save(join(config,'settings.local.json'),{forceLoginMethod:'console'});
  if(variant==='claude-empty-env')save(join(config,'settings.json'),{env:[],forceLoginMethod:'claudeai'});
  if(variant==='claude-invalid-json'){mkdirSync(config);writeFileSync(join(config,'settings.json'),'{');}
  if(variant==='claude-null-settings')save(join(config,'settings.json'),null);
  if(variant==='claude-config-link'){mkdirSync(join(root,'external-config'));symlinkSync(join(root,'external-config'),config,'junction');}
 }
 const retainedCatalog='specs/native/codex-'+version+'/catalog.json';
 const catalog=JSON.parse(readFileSync(existsSync(retainedCatalog)?retainedCatalog:'specs/native/codex-0.142.3/catalog.json','utf8'));catalog.nativeExecutableHash=settings.nativeExecutableHash;
 if(variant==='catalog-hash')catalog.nativeExecutableHash='sha256:'+'0'.repeat(64);
 if(variant==='executable-hash'){settings.nativeExecutableHash='sha256:'+'0'.repeat(64);catalog.nativeExecutableHash=settings.nativeExecutableHash;}
 if(variant==='no-realtime')catalog.clientRequests=catalog.clientRequests.filter(row=>row.method!=='thread/realtime/start');
 if(variant==='catalog-provider')catalog.provider='claude';
 save(join(artifactRoot,'dist','specs','native','codex-'+version,'catalog.json'),catalog);
 if(variant==='fence')save(join(dataRoot,'process-stop-fence.json'),{schemaVersion:1,owner:'native',ownerId:'fixture-epoch',processId:1234,observedAt:'2026-01-02T03:04:05.000Z',code:'outcome_unknown'});
 return {settings,artifactRoot,dataRoot,home,root};
}
if(input.action==='prepare'){process.stdout.write(JSON.stringify(prepare(input.root,input.variant)));}
else if(input.action==='preflight'){
 const {settings,artifactRoot,dataRoot,root}=input;let before=null,epoch=false,error=null;
 try{await startNativeProcess({settings,artifactRoot,dataRoot,clientVersion:'1.2.3',hiveCredential:'fixture-only',beforeLaunch:async home=>{before=relative(root,home).replaceAll('\\','/');throw new IvyError('fixture_preflight_complete','Fixture stops before every CLI probe.');},beginEpoch:()=>{epoch=true;throw Error('Fixture must not launch');},onClose:()=>assert.fail('no process'),onRequest:()=>assert.fail('no process'),onNotification:()=>assert.fail('no process')});}
 catch(e){error=IvyError.from(e).code;}
 process.stdout.write(JSON.stringify({before,epoch,error,homeExists:existsSync(join(dataRoot,'native')),claudeConfigExists:existsSync(join(dataRoot,'claude-config')),adapterHomeExists:existsSync(join(dataRoot,'claude-adapter'))}));
}else if(input.action==='handshake'){
 // Exact Agent-specific original span, including its title, owned-version rule,
 // Claude marker and daemon check. Its file lookup is the real fs operation.
 const source=readFileSync('services/agent-manager/src/process.ts','utf8').replaceAll('\r\n','\n');
 const start=source.indexOf('    const initialized = await rpc.request('),end=source.indexOf('    return {\n      rpc,',start);
 assert(start>=0&&end>start);const original=source.slice(start,end);const executable=original.replace(' as Record<string, Wire.Json>','');
 const run=new Function('rpc','options','settings','expectedHome','external','claude','daemon','realpath','requireThat','IvyError','initializedServerVersion',`return (async()=>{try{${executable}return {initialized:initialized.result,nativeHome:await realpath(String(observed['codexHome'])),actualServerVersion};}catch(error){throw error instanceof IvyError?error:new IvyError('native_start_failed','Public native startup failed.');}})()`);
 const rows=[];
 for(const row of input.rows){const frames=[];let value=null,error=null;
  const rpc={async request(method,params){frames.push({method,params});return row.reply;},notify(method){frames.push({method});}};
  try{value=await run(rpc,{clientVersion:'1.2.3'},{nativeVersion:row.version},realpathSync(row.expectedHome),row.mode==='external',row.mode==='claude',row.daemon,realpath,requireThat,IvyError,initializedServerVersion);}catch(e){error={code:e.code,message:e.message};}
  rows.push({frames,value,error});
 }
 process.stdout.write(JSON.stringify({sourceHash:digest(source),spanHash:digest(original),rows}));
}else{throw Error('Unsupported fixture action');}
