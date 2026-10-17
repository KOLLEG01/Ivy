import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, readFile, readdir, writeFile, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { coreConfiguration, assertLaneIsolation, validateEmittedSettings } from '../fixtures/ivy-rust-lanes.mjs';
import { compareObservation, qualifyPreparedCandidate, runDifferential } from '../../../tools/testing/ivy-rust-differential.mjs';

const make = () => ['a','b'].map((name,index) => ({
  name,
  ...Object.fromEntries([['linux','linux'],['windows','win32']].map(([target,os]) => [target,coreConfiguration({
    os, root:os==='linux'?`/srv/test/${name}`:`D:\\test\\${name}`,hostId:`test-${name}-${os}`,
    publicBaseUrl:`https://192.0.2.2:${41000+index}/ivy`,principalId:`principal-${name}`,
    listenPort:42000+index,executables:os==='linux'?{node:'/tools/node',git:'/tools/git',tar:'/tools/tar'}:{node:'D:\\tools\\node.exe',git:'D:\\tools\\git.exe',tar:'D:\\tools\\tar.exe'},
  },String(index+1).repeat(64))])),
}));

test('paired core configurations validate the actual serialized settings and stay disabled', async () => {
  const { validateHost } = await import('../../../dist/packages/contracts/src/validation.js');
  const lanes = JSON.parse(JSON.stringify(make()));
  assertLaneIsolation(lanes);
  for (const lane of lanes) for (const target of ['linux','windows']) validateEmittedSettings(lane[target],validateHost);
  const instance = lanes[0].linux.instances.find(x=>x.componentId==='hive');
  assert.deepEqual(instance.secretPaths,['/settings/credentials']);
  delete instance.settings;
  assert.throws(()=>validateEmittedSettings(lanes[0].linux,validateHost),{code:'invalid_arguments',outcome:'not_executed'});
});

test('fresh installation retains TaskBoard settings under its component ID after serialization', async () => {
  const { validateTaskBoard } = await import('../../../dist/packages/contracts/src/validation.js');
  await mkdir('.local',{recursive:true});
  const root=await mkdtemp(resolve('.local','ivy-rust-fresh-'));
  try {
    const executables=join(root,'executables.json'),native=join(root,'native.json'),output=join(root,'installation');
    await writeFile(executables,JSON.stringify({executables:{node:process.execPath,git:process.execPath,tar:process.execPath}}));
    await writeFile(native,JSON.stringify({instances:[{componentId:'agent-manager',settings:{nativeExecutable:process.execPath,nativeVersion:'0.159.2',nativeExecutableHash:'sha256:'+'a'.repeat(64)}}]}));
    await promisify(execFile)(process.execPath,['tests/acceptance/scenarios/fresh-config.mjs','--distribution',resolve('.'),'--root',output,'--executables',executables,'--native-config',native],{windowsHide:true,timeout:30000});
    const config=JSON.parse(await readFile(join(output,'config.json'),'utf8'));
    const task=config.instances.find(x=>x.componentId==='task-board');
    validateTaskBoard('Settings',task.settings);
    assert.equal(task.settings.scheduler.enabled,true);assert.equal(task.enabled,false);
    assert.equal(task.settings.principalId,config.instances.find(x=>x.componentId==='hive').settings.credentials[0].principalId);
  } finally {await rm(root,{recursive:true,force:true});}
});

test('configuration CLI writes four private configs once, and its public receipt contains no credential', async () => {
  const { validateHost } = await import('../../../dist/packages/contracts/src/validation.js');
  const root = await mkdtemp(join(tmpdir(),'ivy-rust-config-'));
  try {
    const settings = {};
    for (const [index,name] of ['reference','candidate'].entries()) {
      settings[name] = {};
      for (const target of ['linux','windows']) {
        const config=make()[index][target];
        settings[name][target]={root:config.ivyRoot,hostId:config.hostId,publicBaseUrl:config.publicBaseUrl,executables:config.executables,principalId:`owner-${name}`,listenPort:42000+index};
      }
    }
    const input=join(root,'input.json'), output=join(root,'configs');
    await writeFile(input,JSON.stringify(settings));
    const args=['tests/acceptance/scenarios/ivy-rust-config.mjs','--distribution',resolve('.'),'--settings',input,'--output',output];
    const {stdout}=await promisify(execFile)(process.execPath,args,{windowsHide:true,timeout:30000});
    const receipt=JSON.parse(stdout);assert.equal(receipt.configurations.length,4);
    const lanes=[];
    for(const name of ['reference','candidate']) {
      const lane={name};
      for(const target of ['linux','windows']) {
        lane[target]=JSON.parse(await readFile(join(output,`${name}-${target}.json`),'utf8'));
        validateEmittedSettings(lane[target],validateHost);
        assert.ok(!stdout.includes(lane[target].instances[0].credential));
      }
      lanes.push(lane);
    }
    assertLaneIsolation(lanes);
    await assert.rejects(promisify(execFile)(process.execPath,args,{windowsHide:true,timeout:30000}));
    // Exercise real private evidence setup without dispatching a network request.
    const corpus=join(root,'corpus.json'), evidence=join(root,'evidence');
    await writeFile(corpus,JSON.stringify({schemaVersion:1,sourceCommit:'fixture',contractHash:'fixture',
      origins:lanes.map(lane=>new URL(lane.linux.publicBaseUrl).origin),externalEffects:'local-fixtures-only',
      steps:[{id:'invalid',params:{},expected:{}}]}));
    const differentialArgs=['tests/acceptance/scenarios/ivy-rust-differential.mjs','--distribution',resolve('.'),
      '--reference',join(output,'reference-linux.json'),'--candidate',join(output,'candidate-linux.json'),
      '--configuration-receipt',join(output,'receipt.json'),'--corpus',corpus,'--evidence',evidence];
    await assert.rejects(promisify(execFile)(process.execPath,differentialArgs,{windowsHide:true,timeout:30000}),
      error=>/Each step needs a request and independent expected result/.test(error.stderr));
    assert.deepEqual(await readdir(evidence),['inputs.json']);
    const original=await readFile(join(evidence,'inputs.json'),'utf8');
    assert.equal(JSON.parse(original).sourceCommit,'fixture');
    await assert.rejects(promisify(execFile)(process.execPath,differentialArgs,{windowsHide:true,timeout:30000}),
      error=>/target_conflict/.test(error.stderr));
    assert.equal(await readFile(join(evidence,'inputs.json'),'utf8'),original);
  } finally { await rm(root,{recursive:true,force:true}); }
});

test('lab isolation rejects duplicate owner identity, cross-lane credentials and nested roots', () => {
  for (const mutate of [
    lanes=>{lanes[1].windows.hostId=lanes[0].windows.hostId;},
    lanes=>{lanes[1].linux.instances[1].credential=lanes[0].linux.instances[1].credential;},
    lanes=>{lanes[1].linux.ivyRoot='/srv/test/a/nested';},
    lanes=>{lanes[0].windows.runtimeRoot='C:\\existing\\runtime';},
    lanes=>{lanes[0].linux.instances[0].enabled=true;},
    lanes=>{lanes[1].linux.publicBaseUrl=lanes[0].linux.publicBaseUrl;},
  ]) { const lanes=make();mutate(lanes);assert.throws(()=>assertLaneIsolation(lanes)); }
});

test('candidate transfer preserves exact OS, architecture, Node and build identity', () => {
  const runtime={os:'linux',arch:'x64',node:'v24.18.0'};
  const candidate={candidateId:'build',buildId:'build',platform:runtime};
  qualifyPreparedCandidate(candidate,runtime);
  for (const delta of [{os:'win32'},{arch:'arm64'},{node:'v24.18.1'}]) assert.throws(()=>qualifyPreparedCandidate(candidate,{...runtime,...delta}));
  assert.throws(()=>qualifyPreparedCandidate({...candidate,buildId:'different'},runtime));
});

test('observation comparison never erases operation hashes, errors, sequence or unknown outcomes', () => {
  const value={result:{observedAt:'first',operationId:'original',hash:'abc',sequence:3,outcome:'unknown'}};
  compareObservation(value,{result:{...value.result,observedAt:'second'}},['/result/observedAt']);
  for (const key of ['operationId','hash','sequence','outcome']) {
    assert.throws(()=>compareObservation(value,{result:{...value.result,[key]:'changed'}},['/result/observedAt']));
    assert.throws(()=>compareObservation(value,value,[`/result/${key}`]));
  }
  assert.throws(()=>compareObservation(value,value,['/result/pid']));
});

test('differential calls save original request first and require independent expected success', async () => {
  const events=[];
  const client={request:async(method,params)=>{events.push(['call',method,params]);return {count:1};}};
  const step={id:'read',method:'sample.read',params:{operationId:'original'},expected:{result:{count:1}}};
  const result=await runDifferential({steps:[step],reference:client,candidate:client,record:async(name,value)=>events.push([name,value])});
  assert.deepEqual(result,[{id:'read',passed:true}]);
  assert.equal(events[0][0],'read-reference-request');assert.equal(events[3][0],'read-candidate-request');
  await assert.rejects(runDifferential({steps:[{...step,expected:{result:{count:2}}}],reference:client,candidate:client,record:async()=>{}}));
});

test('lost response stops before candidate dispatch and never retries the mutation', async () => {
  let calls=0;const records=[];
  const client={request:async()=>{calls++;throw Object.assign(new Error('lost'),{code:'connection_lost',outcome:'unknown'});}};
  await assert.rejects(runDifferential({steps:[{id:'write',method:'sample.write',params:{operationId:'same'},expected:{result:{}}}],reference:client,candidate:client,record:async(name,value)=>records.push([name,value])}),/reconcile its original request/);
  assert.equal(calls,1);assert.equal(records.at(-1)[1].outcome,'unknown');assert.equal(records[0][1].params.operationId,'same');
});

test('equal domain failures pass only when that failure was explicitly expected', async () => {
  const client={request:async()=>{throw Object.assign(new Error('no'),{code:'conflict',outcome:'not_executed'});}};
  const step={id:'conflict',method:'sample.write',params:{},expected:{error:{code:'conflict',outcome:'not_executed',message:'no',details:null,httpStatus:null,rpcCode:null}}};
  await runDifferential({steps:[step],reference:client,candidate:client,record:async()=>{}});
  await assert.rejects(runDifferential({steps:[{...step,expected:{result:{}}}],reference:client,candidate:client,record:async()=>{}}));
});
