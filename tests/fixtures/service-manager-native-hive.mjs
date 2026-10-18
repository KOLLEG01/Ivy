import assert from 'node:assert/strict';
import {readFile, mkdir, copyFile, writeFile, rm} from 'node:fs/promises';
import {join} from 'node:path';
import {createServer} from 'node:net';
import {spawn, spawnSync} from 'node:child_process';
import {setTimeout as delay} from 'node:timers/promises';
import {HiveServer} from '../../services/hive/src/server.ts';
import {HiveClient, discover, callBound, newOperationId} from '../../packages/sdk/src/client.ts';
import {HostJournal} from '../../packages/host-runtime/src/journal.ts';
import {atomicJson} from '../../packages/host-runtime/src/config.ts';
import {hostConfig} from '../../packages/host-runtime/src/host-config.ts';
import {digest} from '../../packages/contracts/src/canonical.ts';

const [binary, root, job] = process.argv.slice(2);
const token = 'isolated-native-manager-fixture';
const hash = digest('isolated-manager-artifact');
const configPath = join(root, 'host.json');
const artifact = join(root, 'artifacts', 'manager');
const managerData = join(root, 'manager-data');
const instancePath = join(root, 'manager.json');
const results = [];
async function until(work, timeout = 15000) {
  const deadline = Date.now() + timeout;
  for (;;) {
    const value = await work();
    if (value) return value;
    assert.ok(Date.now() < deadline, 'Native manager integration did not converge');
    await delay(40);
  }
}
const listener = createServer();
await new Promise(resolve => listener.listen(0, '127.0.0.1', resolve));
const port = listener.address().port;
await new Promise(resolve => listener.close(resolve));
const publicBaseUrl = `http://127.0.0.1:${port}/ivy`;
const gitProbe = spawnSync(process.platform === 'win32' ? 'where.exe' : 'which', ['git'], {encoding:'utf8', windowsHide:true});
assert.equal(gitProbe.status, 0);
const config = {
  schemaVersion:1, hostId:'native-manager-host', publicBaseUrl,
  runtimeRoot:join(root,'state'), artifactRoot:join(root,'artifacts'), stagingRoot:join(root,'staging'),
  executables:{node:process.execPath, git:gitProbe.stdout.trim().split(/\r?\n/)[0]},
  instances:[
    {instanceId:'fixture', serviceNodeId:'native.fixture', componentId:'fixture', engine:'process', enabled:true, settings:{}},
    {instanceId:'manager', serviceNodeId:'native.manager', componentId:'service-manager', engine:'process', enabled:true, credential:token, settings:{hostConfigPath:configPath}},
  ],
};
await atomicJson(configPath, config);
const accepted = await hostConfig(configPath);
const journal = new HostJournal(accepted);
await mkdir(join(artifact,'dist','native'), {recursive:true});
if (process.platform === 'win32') await copyFile(job, join(artifact,'dist','native','ivy-job.exe'));
await atomicJson(join(artifact,'dist','build-info.json'), {version:'1.0.0', buildId:hash});
const instance = {schemaVersion:1, hostId:config.hostId, instanceId:'manager', serviceNodeId:'native.manager', componentId:'service-manager',
  publicBaseUrl, credential:token, version:'1.0.0', buildId:hash, dataRoot:managerData, artifactRoot:artifact, settings:{hostConfigPath:configPath}};
await atomicJson(instancePath, instance);
const candidateId = digest('isolated-fixture-candidate');
const candidate = {candidateId, componentId:'fixture', artifactRoot:join(root,'artifacts','fixture'), manifestPath:join(root,'artifacts','fixture','manifest.json'),
  buildId:candidateId, createdAt:new Date().toISOString(), platform:{os:process.platform, arch:process.arch, node:process.versions.node}};
journal.saveCandidate(candidate, {schemaVersion:1, componentId:'fixture', kind:'native', version:'1.0.0', buildId:candidateId, connectsToHive:false,
  requirements:{node:'>=24.18.0 <25.0.0', hiveProtocol:null, contracts:[]}, entrypoint:{executable:'node',args:['dist/main.mjs'],timeoutMs:1000},
  readiness:{timeoutMs:1000,command:{executable:'node',args:['-e','process.exit(0)'],timeoutMs:1000}},
  shutdown:{timeoutMs:1000}, restart:{policy:'never',minimumDelayMs:100,maximumDelayMs:1000}, storage:{writeFormat:1,minReadableFormat:1,maxReadableFormat:1}});
journal.setInstalled({instanceId:'fixture', candidateId, buildId:candidateId, enabled:true, installedAt:new Date().toISOString()});
const hive = new HiveServer({filename:join(root,'hive.sqlite'), packageRoot:join(root,'packages'), publicBaseUrl, listenHost:'127.0.0.1', listenPort:port,
  version:'test', buildId:digest('isolated-hive'), credentials:[{principalId:'manager',digest:digest(token)}]});
let manager;
let executorTimer;
async function start() {
  let stderr = '';
  const child = spawn(binary, ['--config', instancePath], {env:{...process.env,IVY_NODE_EXECUTABLE:process.execPath,IVY_LAUNCH_ID:'integration-launch'},
    windowsHide:true, stdio:['ignore','ignore','pipe']});
  child.stderr.on('data', data => {stderr = (stderr + data).slice(-32768);});
  const completion = new Promise((resolve,reject) => {child.once('error',reject);child.once('exit',(code,signal) => resolve({code,signal}));});
  manager = {child,completion,stderr:()=>stderr};
  const health = await until(async () => {
    assert.equal(child.exitCode, null, stderr);
    return readFile(join(managerData,'health.json'),'utf8').then(JSON.parse).then(h => h.ready && h).catch(()=>false);
  });
  assert.equal(health.pid, child.pid, 'Health identifies the actual Rust process');
  assert.equal(health.launchId,'integration-launch');
}
async function stop() {
  if (!manager) return;
  const current = manager;
  try {
    if (current.child.exitCode === null) {
      const health = JSON.parse(await readFile(join(managerData,'health.json'),'utf8'));
      await atomicJson(join(managerData,'control.json'), {schemaVersion:1,instanceId:'manager',launchId:'integration-launch',bootId:health.bootId,action:'shutdown',requestedAt:new Date().toISOString()});
    }
    const ended = await Promise.race([current.completion, delay(10000).then(()=>{throw Error('Native manager shutdown deadline exceeded');})]);
    assert.equal(ended.code,0,current.stderr());
    const final = JSON.parse(await readFile(join(managerData,'health.json'),'utf8'));
    assert.equal(final.ready,false);assert.equal(final.details,'ServiceManager stopped.');
  } finally {
    if (current.child.exitCode === null) {
      if (process.platform === 'win32') spawnSync('taskkill.exe',['/PID',String(current.child.pid),'/T','/F'],{windowsHide:true});
      else current.child.kill('SIGKILL');
      await current.completion;
    }
    manager = null;
  }
}
try {
  await hive.start();
  const client = new HiveClient(publicBaseUrl,{credential:token});
  const originalConfiguration = await client.request('hostConfigurations.put',{hostId:config.hostId,configuration:config,mutationId:await newOperationId(client)});
  const executor = async () => atomicJson(join(root,'state','executor.json'), {schemaVersion:1,hostId:config.hostId,pid:process.pid,bootId:'executor-fixture',buildId:null,
    state:'ready',activeDeploymentId:null,code:null,observedAt:new Date().toISOString()});
  await executor();let executorWork = Promise.resolve();
  executorTimer = setInterval(()=>{executorWork=executorWork.then(executor);void executorWork.catch(()=>{});},250);
  await start();
  const bindings = new Map();
  const call = async (name,args={},operation=args.operationId) => {
    if (!bindings.has(name)) bindings.set(name,await discover(client,'host.'+name,{serviceNodeId:'native.manager'}));
    return callBound(client,bindings.get(name),args,operation);
  };
  const deploy = {instanceId:'fixture',candidateId,operationId:'native-deploy'};
  await assert.rejects(call('deploy',deploy,'other-operation'), e=>e.code==='mutation_conflict');
  assert.equal(journal.operation(deploy.operationId),null);
  const record = await call('deploy',deploy);
  assert.deepEqual(journal.get(record.deploymentId).record,record);
  journal.advance(record.deploymentId,record.phase,'failed',{errorCode:'fixture_terminal_failure'});
  const terminal = await call('deploy',deploy);
  assert.equal(terminal.deploymentId,record.deploymentId);assert.equal(terminal.phase,'failed');
  await assert.rejects(call('deploy',{instanceId:'fixture',operationId:deploy.operationId,source:join(root,'missing-source')}),e=>e.code==='mutation_conflict');
  results.push('routed operation binding and terminal replay');
  for (const action of ['rollback','restart','enable','disable']) {
    const args = {instanceId:'fixture',operationId:'native-'+action,...(action==='rollback'?{targetBuild:candidateId}:{})};
    const value = await call(action,args);
    assert.equal(journal.get(value.deploymentId).request.action,action);assert.equal(value.instanceId,'fixture');
    journal.advance(value.deploymentId,value.phase,'failed',{errorCode:'fixture_terminal_failure'});
    assert.equal((await call(action,args)).deploymentId,value.deploymentId);
  }
  results.push('all five real mutation handlers and durable records');
  const checkout = join(root,'checkout');
  await mkdir(join(checkout,'packages'),{recursive:true});
  await writeFile(join(checkout,'package.json'),'{"name":"isolated-source","version":"1.0.0"}\n');
  await writeFile(join(checkout,'package-lock.json'),'{"lockfileVersion":3}\n');
  await writeFile(join(checkout,'packages','bytes.txt'),'accepted bytes\r\n');
  assert.equal(spawnSync(config.executables.git,['init','--quiet'],{cwd:checkout,windowsHide:true}).status,0);
  const sourceArgs = {instanceId:'fixture',operationId:'native-source',source:checkout};
  const sourceRecord = await call('deploy',sourceArgs);
  const sourceEntry = journal.get(sourceRecord.deploymentId);
  assert.equal(await readFile(join(sourceEntry.sourceSnapshot.sourceRoot,'packages','bytes.txt'),'utf8'),'accepted bytes\r\n');
  await rm(checkout,{recursive:true});
  assert.equal((await call('deploy',sourceArgs)).deploymentId,sourceRecord.deploymentId);
  journal.advance(sourceRecord.deploymentId,sourceRecord.phase,'failed',{errorCode:'fixture_terminal_failure'});
  assert.equal((await call('deployment',{deploymentId:sourceRecord.deploymentId})).record.phase,'failed');
  await assert.rejects(call('deployment',{deploymentId:'missing'}),e=>e.code==='not_found');
  const status = await call('status');assert.equal(status.hostId,config.hostId);assert.ok(!JSON.stringify(status).includes(token));
  results.push('fixed source bytes, source-loss replay, status and deployment');
  await until(async()=> (await client.request('deployments.list',{hostId:config.hostId})).items.some(i=>i.record.deploymentId===record.deploymentId));
  const beforeOwner = journal.observations().fixture?.ownerBootId;
  const next = structuredClone(config);
  next.instances.push({instanceId:'added',serviceNodeId:'native.added',componentId:'fixture',engine:'process',enabled:false,settings:{}});
  await client.request('hostConfigurations.put',{hostId:config.hostId,expectedRevision:originalConfiguration.revision,configuration:next,mutationId:await newOperationId(client)});
  await until(()=>readFile(join(root,'state','desired-state-hint.json'),'utf8').then(JSON.parse).then(h=>h.configuration).catch(()=>false));
  assert.ok(!(await call('status')).instances.some(i=>i.instanceId==='added'),'Hive hint alone is not accepted local state');
  await atomicJson(configPath,next);
  await until(async()=> (await call('status')).instances.some(i=>i.instanceId==='added'));
  if (process.platform==='win32') {
    await until(()=>journal.observations().added?.state==='idle');
    assert.equal(journal.observations().fixture.ownerBootId,beforeOwner);
  }
  results.push('real Hive reports and hint, independent local topology adoption');
  await atomicJson(configPath,{...next,hostId:'another-host'});
  await assert.rejects(call('restart',{instanceId:'fixture',operationId:'wrong-local-host'}),e=>e.code==='target_conflict');
  assert.equal(journal.operation('wrong-local-host'),null);
  await atomicJson(configPath,next);
  // A wrong boot must leave the Rust process alive.
  await atomicJson(join(managerData,'control.json'),{schemaVersion:1,instanceId:'manager',launchId:'integration-launch',bootId:'wrong-boot',action:'shutdown',requestedAt:new Date().toISOString()});
  await delay(650);assert.equal(manager.child.exitCode,null);
  await stop();
  await start();bindings.clear();
  const replay = await call('deploy',deploy);assert.equal(replay.deploymentId,record.deploymentId);assert.equal(replay.phase,'failed');
  await stop();
  results.push('host reload refusal, control binding, process replacement replay');
  console.log(JSON.stringify({ok:true,results,limitations:['Executor status is a fixture; no native deployment executor or archive/platform acceptance.']}));
} finally {
  clearInterval(executorTimer);
  try {await stop();} finally {await hive.close();journal.close();}
}
