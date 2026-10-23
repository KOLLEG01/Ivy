import assert from 'node:assert/strict';
import {readFile, mkdir, copyFile, writeFile, rm, cp} from 'node:fs/promises';
import {join, resolve, toNamespacedPath} from 'node:path';
import {createServer} from 'node:net';
import {spawn, spawnSync} from 'node:child_process';
import {setTimeout as delay} from 'node:timers/promises';
import {HiveServer} from '../../services/hive/src/server.ts';
import {HiveClient, discover, callBound, newOperationId} from '../../packages/sdk/src/client.ts';
import {HostJournal} from '../../packages/host-runtime/src/journal.ts';
import {atomicJson} from '../../packages/host-runtime/src/config.ts';
import {hostConfig} from '../../packages/host-runtime/src/host-config.ts';
import {runtimeResetMarker} from '../../packages/host-runtime/src/runtime-maintenance.ts';
import {digest} from '../../packages/contracts/src/canonical.ts';
import {materializeNativePlan} from '../../packages/host-runtime/src/native-plan.mjs';

const [binary, root, job, ownerBinary, healthBinary, mode = 'full', identityPath] = process.argv.slice(2);
assert.ok(mode === 'full' || mode === 'dispatcher-only');
const releaseIdentity = mode === 'dispatcher-only' ? JSON.parse(await readFile(identityPath, 'utf8')) : null;
if (releaseIdentity) {
  assert.equal(process.platform, 'win32');
  assert.equal(releaseIdentity.schemaVersion, 1);
  assert.match(releaseIdentity.buildId, /^sha256:[0-9a-f]{64}$/);
  assert.equal(releaseIdentity.version, JSON.parse(await readFile('package.json', 'utf8')).version);
}
const token = 'isolated-native-manager-fixture';
const hash = releaseIdentity?.buildId ?? digest('isolated-manager-artifact');
const version = releaseIdentity ? JSON.parse(await readFile('services/service-manager/deploy.json', 'utf8')).version : '1.0.0';
const configPath = join(root, 'host.json');
const artifact = join(root, 'artifacts', 'manager');
const managerData = join(root, 'state', 'instances', 'manager', 'data');
const instancePath = join(root, 'state', 'manager.json');
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
await atomicJson(join(artifact,'dist','build-info.json'), {schemaVersion:1,componentId:'service-manager',version, buildId:hash});
const instance = {schemaVersion:1, hostId:config.hostId, instanceId:'manager', serviceNodeId:'native.manager', componentId:'service-manager',
  publicBaseUrl, credential:token, version, buildId:hash, dataRoot:managerData, artifactRoot:artifact, settings:{hostConfigPath:configPath}};
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
let dispatcherOwner;
let dispatcherProof;
let executorTimer;

// Read only the exact owned PID and its parents; never enumerate other trees.
function windowsProcess(pid) {
  assert.ok(Number.isSafeInteger(pid) && pid > 0);
  const script = `$ErrorActionPreference='Stop';$p=Get-CimInstance Win32_Process -Filter 'ProcessId=${pid}';if($null -eq $p){'null'}else{$p|Select-Object ProcessId,ParentProcessId,ExecutablePath|ConvertTo-Json -Compress}`;
  const result = spawnSync('powershell.exe',['-NoProfile','-NonInteractive','-EncodedCommand',Buffer.from(script,'utf16le').toString('base64')],
    {encoding:'utf8',windowsHide:true,timeout:10000});
  assert.equal(result.status,0,result.stderr);
  return JSON.parse(result.stdout.trim());
}

async function qualifyDispatcherOwner() {
  const dist = join(artifact,'dist');
  const payload = join(dist,'native',`${process.platform}-${process.arch}`);
  await mkdir(payload,{recursive:true});
  await copyFile(binary,join(payload,'ivy-service-manager.exe'));
  await copyFile(healthBinary,join(payload,'ivy-health.exe'));
  await copyFile(job,join(payload,'ivy-job.exe'));
  await atomicJson(join(payload,'build-info.json'),{schemaVersion:1,componentId:'service-manager',version:instance.version,buildId:hash});
  await cp('specs/native',join(dist,'specs/native'),{recursive:true});
  const plan = materializeNativePlan(JSON.parse(await readFile('services/service-manager/deploy.json','utf8')));
  const manifest = Object.fromEntries(['schemaVersion','componentId','kind','connectsToHive','requirements','entrypoint','readiness','shutdown','restart','storage','runtimeReset']
    .map(key=>[key,plan[key]]));
  Object.assign(manifest,{version:instance.version,buildId:hash});
  await atomicJson(join(artifact,'manifest.json'),manifest);
  journal.saveCandidate({candidateId:hash,componentId:'service-manager',artifactRoot:artifact,manifestPath:join(artifact,'manifest.json'),
    buildId:hash,createdAt:new Date().toISOString(),platform:{os:process.platform,arch:process.arch,node:process.versions.node}},manifest);
  const ownerPath = join(root,'dispatcher-owner.json'), closePath = join(root,'dispatcher-owner-close'), replacementPath = join(root,'dispatcher-replacement.json');
  const target = {schemaVersion:1,instanceId:'manager',revision:'dispatcher-owner-launch',candidateId:hash,desired:'running',
    configPath:instancePath,requestedAt:new Date().toISOString()};
  await atomicJson(ownerPath,{hostConfigPath:configPath,bootstrapRoot:artifact,closePath,replacementPath,target});
  let diagnostics = '';
  const child = spawn(ownerBinary,['--exact','dispatcher_owner_fixture','--nocapture'],{env:{...process.env,IVY_MANAGER_DISPATCH_OWNER:ownerPath},
    windowsHide:true,stdio:['ignore','pipe','pipe']});
  for (const stream of [child.stdout,child.stderr]) stream.on('data',bytes=>{diagnostics=(diagnostics+bytes).slice(-32768);});
  const completion = new Promise((resolve,reject)=>{child.once('error',reject);child.once('exit',(code,signal)=>resolve({code,signal}));});
  dispatcherOwner = {child,completion,closePath};
  const ready = await until(()=>{
    assert.equal(child.exitCode,null,diagnostics);
    const value = journal.observations().manager;
    assert.notEqual(value?.state,'failed',JSON.stringify(value));
    return value?.state==='ready' && value.health?.launchId===target.revision && value;
  },25000);
  const health = ready.health;
  assert.equal(ready.ownerPid,child.pid);
  assert.notEqual(health.pid,child.pid,'Rust service health is distinct from its RuntimeOwner');
  const native = windowsProcess(health.pid), helper = windowsProcess(native.ParentProcessId);
  const samePath = (left,right)=>assert.equal(toNamespacedPath(resolve(left)).toLowerCase(),toNamespacedPath(resolve(right)).toLowerCase());
  samePath(native.ExecutablePath,join(payload,'ivy-service-manager.exe'));
  samePath(helper.ExecutablePath,join(payload,'ivy-job.exe'));
  assert.equal(helper.ParentProcessId,child.pid);
  assert.notEqual(health.pid,helper.ProcessId,'Health identifies the native child of the Job');
  assert.equal(health.buildId,hash);assert.ok(health.bootId);
  dispatcherProof = {health,native,helper};

  // The manifest health command is a direct read-only probe, with no service Job.
  const controlPath = join(managerData,'control.json');
  const readControl = () => readFile(controlPath,'utf8').catch(error => {
    if (error.code === 'ENOENT') return null;
    throw error;
  });
  const beforeControl = await readControl();
  const probe = spawn(join(artifact,plan.readiness.command.executable),[...plan.readiness.command.args,'--config',instancePath],
    {cwd:artifact,env:{...process.env,IVY_NODE_EXECUTABLE:'intentionally-invalid-node'},windowsHide:true,stdio:['ignore','pipe','pipe']});
  let stdout='',stderr='';probe.stdout.on('data',bytes=>{stdout+=bytes;});probe.stderr.on('data',bytes=>{stderr+=bytes;});
  const probeExit = await new Promise((resolve,reject)=>{probe.once('error',reject);probe.once('close',code=>resolve(code));});
  assert.equal(probeExit,0,stderr);assert.equal(stderr,'');
  const observed = JSON.parse(stdout.trim());
  assert.deepEqual(observed,{ok:true,buildId:hash,generation:health.generation});
  assert.equal(await readControl(),beforeControl);
  results.push('real RuntimeOwner/Job/native Manager chain and direct native health');

  await rm(join(payload,'ivy-health.exe'));
  await atomicJson(replacementPath,{...target,revision:'incomplete-native-replacement',requestedAt:new Date().toISOString()});
  await until(()=>{
    assert.equal(child.exitCode,null,diagnostics);
    const value=journal.observations().manager;
    return value?.state==='failed' && value.code==='artifact_missing';
  });
  const retained=JSON.parse(await readFile(join(managerData,'health.json'),'utf8'));
  assert.equal(retained.pid,health.pid);assert.equal(retained.bootId,health.bootId);assert.equal(retained.launchId,target.revision);
  assert.equal(await readControl(),beforeControl);
  assert.ok(windowsProcess(health.pid));
  results.push('frozen-manifest native preflight refuses missing payload before draining the ready Manager');

  // A mismatched boot/launch cannot stop the actual native child through its wrapper.
  await atomicJson(controlPath,{schemaVersion:1,instanceId:'manager',launchId:'wrong-launch',bootId:health.bootId,action:'shutdown',requestedAt:new Date().toISOString()});
  await delay(650);assert.equal(child.exitCode,null,diagnostics);
  assert.ok(windowsProcess(health.pid));
  await writeFile(closePath,'close');
  const ended = await Promise.race([completion,delay(20000,undefined,{ref:false}).then(()=>{throw Error('Dispatcher RuntimeOwner cleanup deadline exceeded');})]);
  assert.equal(ended.code,0,diagnostics);
  const final = JSON.parse(await readFile(join(managerData,'health.json'),'utf8'));
  assert.equal(final.ready,false);assert.equal(final.bootId,health.bootId);assert.equal(final.details,'ServiceManager stopped.');
  const control = JSON.parse(await readFile(controlPath,'utf8'));
  assert.equal(control.launchId,target.revision);assert.equal(control.bootId,health.bootId);
  for (const pid of [health.pid,helper.ProcessId]) assert.equal(windowsProcess(pid),null,'Owned native tree survived cleanup');
  dispatcherProof.retiredPids = [health.pid,helper.ProcessId];
  assert.equal(journal.observations().manager.state,'stopped');
  dispatcherOwner = null;
  results.push('RuntimeOwner cooperative boot-bound shutdown retires the complete native Job tree');
}

async function stopDispatcherOwner() {
  if (!dispatcherOwner) return;
  const current = dispatcherOwner;
  await writeFile(current.closePath,'close');
  try {
    await Promise.race([current.completion,delay(20000,undefined,{ref:false}).then(()=>{throw Error('Dispatcher owner cleanup exceeded its deadline');})]);
  } finally {
    if (current.child.exitCode===null) {
      spawnSync('taskkill.exe',['/PID',String(current.child.pid),'/T','/F'],{windowsHide:true});
      await current.completion;
    }
    dispatcherOwner = null;
  }
}
async function start() {
  let stderr = '';
  const environment = {...process.env, IVY_LAUNCH_ID:'integration-launch'};
  delete environment.IVY_NODE_EXECUTABLE;
  if (process.platform === 'win32') environment.PATH = join(process.env.SystemRoot, 'System32');
  const child = spawn(binary, ['--config', instancePath], {env:environment,
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
  if (mode === 'full') {
  const rejectedStart = code => {
    const output = spawnSync(binary,['--config',instancePath],{env:{...process.env,IVY_NODE_EXECUTABLE:process.execPath,IVY_LAUNCH_ID:'failed-start'},
      windowsHide:true,encoding:'utf8',timeout:10000});
    assert.equal(output.status,1,output.stderr);assert.equal(output.stdout,'');
    assert.deepEqual(JSON.parse(output.stderr.trim()),{code,message:'ServiceManager failed to start.'});
  };
  const marker = runtimeResetMarker(accepted);
  assert.equal(marker,join(root,'reset','maintenance.json'));
  await mkdir(join(root,'reset'),{recursive:true});await writeFile(marker,'{}');
  rejectedStart('maintenance_active');await rm(marker);
  await atomicJson(instancePath,{...instance,serviceNodeId:'unconfigured.manager'});
  rejectedStart('target_conflict');await atomicJson(instancePath,instance);
  await mkdir(join(managerData,'health.json'),{recursive:true});
  rejectedStart('internal_error');await rm(join(managerData,'health.json'),{recursive:true});
  results.push('actual reset and membership refusal, failed initial-health cleanup');
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
  await assert.rejects(call('restart',{instanceId:'fixture',operationId:'wrong-local-host'},'different-route'),e=>e.code==='mutation_conflict');
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
  }
  if (process.platform==='win32') await qualifyDispatcherOwner();
  console.log(JSON.stringify({ok:true,results,...(releaseIdentity ? {mode,identity:releaseIdentity,componentStamp:{schemaVersion:1,componentId:'service-manager',version,buildId:hash},processProof:dispatcherProof} : {}),limitations:['Executor status is a fixture; no native deployment executor or archive/platform acceptance.']}));
} finally {
  clearInterval(executorTimer);
  try {await stopDispatcherOwner();await stop();} finally {await hive.close();journal.close();}
}
