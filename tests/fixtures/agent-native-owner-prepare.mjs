// Synthetic filesystem setup only; startup outcomes come from fixed reference cases.
import {readFileSync,mkdirSync,writeFileSync,existsSync,symlinkSync} from 'node:fs';
import {join,dirname} from 'node:path';
import {createHash} from 'node:crypto';
const digest=bytes=>'sha256:'+createHash('sha256').update(bytes).digest('hex');
let bytes='';for await(const chunk of process.stdin)bytes+=chunk;const input=JSON.parse(bytes);
const limits={maxOperations:100,maxJournalBytes:1048576,maxPendingInputs:8,maxNotificationBytes:1048576};
const save=(path,value)=>{mkdirSync(dirname(path),{recursive:true});writeFileSync(path,JSON.stringify(value));};
function prepare(root,variant){
 mkdirSync(root,{recursive:true});
 const artifactRoot=join(root,'artifact'),dataRoot=join(root,'data'),bin=join(root,'bin');
 mkdirSync(bin);mkdirSync(dataRoot);mkdirSync(artifactRoot);
 const executable=join(bin,variant==='wrapper'?'native.CMD':'native.exe');writeFileSync(executable,'pinned synthetic file; never execute\n');
 const home=join(dataRoot,'native');
 const lineEndings={'version-lf':'\n','version-cr':'\r','version-ls':'\u2028','version-ps':'\u2029'};
 const version=Object.hasOwn(lineEndings,variant)?'0.142.3'+lineEndings[variant]:variant.startsWith('version-')?variant.slice(8):'0.142.3';
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
  const adapter=join(root,'adapter'),source=join(adapter,'bridge');mkdirSync(source,{recursive:true});
  writeFileSync(join(adapter,'package-lock.json'),'{}\n');writeFileSync(join(source,'main.mjs'),'// pinned fixture\n');writeFileSync(join(source,'session.mjs'),'// extra\n');
  const manifest=['bridge/main.mjs','bridge/session.mjs','package-lock.json'].map(name=>name+'\0'+digest(readFileSync(join(adapter,name))).slice(7)+'\0').join('');
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
 save(join(artifactRoot,'dist','specs','native','codex-'+(existsSync(retainedCatalog)?version:'0.142.3'),'catalog.json'),catalog);
 if(variant==='fence')save(join(dataRoot,'process-stop-fence.json'),{schemaVersion:1,owner:'native',ownerId:'fixture-epoch',processId:1234,observedAt:'2026-01-02T03:04:05.000Z',code:'outcome_unknown'});
 return {settings,artifactRoot,dataRoot,home,root};
}
process.stdout.write(JSON.stringify(prepare(input.root,input.variant)));
