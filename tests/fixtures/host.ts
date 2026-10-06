import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm, copyFile } from 'node:fs/promises';
import { join, relative, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { pathToFileURL } from 'node:url';
import { spawnSync } from 'node:child_process';
import { setTimeout as delay } from 'node:timers/promises';
import { captureSource } from '../../packages/host-runtime/src/source.js';
import { prepareCandidate } from '../../packages/host-runtime/src/prepare.js';
import { runCommand } from '../../packages/host-runtime/src/process.js';
import { atomicJson } from '../../packages/host-runtime/src/config.js';
import { HostJournal } from '../../packages/host-runtime/src/journal.js';
import type { Host } from '../../packages/contracts/src/generated.js';

export async function until(check: () => boolean | Promise<boolean>, timeout = 15_000): Promise<void> {
  const end = Date.now() + timeout;
  while (!(await check())) { assert.ok(Date.now() < end, 'condition did not converge before deadline'); await delay(50); }
}
export async function hostFixture(t: TestContext, componentId = 'fixture') {
  const root = await mkdtemp(join(tmpdir(), 'ivy-executor-test-')), source = join(root, 'checkout');
  const exe = (name: string) => spawnSync(process.platform === 'win32' ? 'where.exe' : 'which', [name], { encoding: 'utf8', windowsHide: true, timeout: 5000 }).stdout.trim().split(/\r?\n/)[0]!;
  const config: Host.HostConfig = { schemaVersion: 1, hostId: 'executor-fixture', runtimeRoot: join(root, 'state'), artifactRoot: join(root, 'artifacts'), stagingRoot: join(root, 'staging'), publicBaseUrl: 'http://127.0.0.1:39081/ivy',
    executables: { git: exe('git'), tar: exe('tar'), node: process.execPath }, instances: ['first', 'unrelated'].map(instanceId => ({ instanceId, serviceNodeId: instanceId, componentId, enabled: true, engine: 'process' as const, settings: {} })) };
  const configPath = join(root, 'config.json'); await atomicJson(configPath, config);
  for (const folder of ['packages', 'tools', join('services', componentId)]) await mkdir(join(source, folder), { recursive: true });
  await atomicJson(join(source, 'package.json'), { name: 'executor-fixture', version: '1.0.0', private: true, type: 'module', packageManager: 'npm@11.16.0' });
  await atomicJson(join(source, 'package-lock.json'), { name: 'executor-fixture', version: '1.0.0', lockfileVersion: 3 });
  await writeFile(join(source, '.gitignore'), 'config.json\ndist/\nnode_modules/\n');
  const buildScript = ['tools', 'build.mjs'].join('/');
  await writeFile(join(source, buildScript), 'import fs from "node:fs/promises";await fs.mkdir("dist",{recursive:true});for(const name of ["main.mjs","atomic-file.mjs"])await fs.copyFile("packages/"+name,"dist/"+name);if("' + componentId + '"==="host-executor"){await fs.mkdir("dist/native",{recursive:true});for(const name of ["ivy-job.exe","ivy-host-job.exe"])await fs.writeFile("dist/native/"+name,"fixture-only");}');
  // The actual shared replacement implementation becomes an explicitly hashed fixture input.
  await copyFile(resolve('dist/packages/host-runtime/src/atomic-file.js'), join(source, 'packages/atomic-file.mjs'));
  const plan: Host.BuildPlan = { schemaVersion: 1, componentId, kind: 'native', version: '1.0.0', description: 'Isolated real process lifecycle fixture; never performs external domain actions.', connectsToHive: false,
    requirements: { node: '>=24.18.0 <25.0.0', hiveProtocol: null, contracts: [] }, entrypoint: { executable: 'node', args: ['dist/main.mjs'], timeoutMs: 5000 },
    prepare: [{ executable: 'node', args: [buildScript], timeoutMs: 5000 }], checks: [],
    readiness: { timeoutMs: 7000, command: { executable: 'node', args: ['-e', 'const fs=require("node:fs"),p=require("node:path"),c=JSON.parse(fs.readFileSync(process.env.IVY_INSTANCE_CONFIG));if(!JSON.parse(fs.readFileSync(p.join(c.dataRoot,"health.json"))).ready)process.exit(1)'], timeoutMs: 5000 } },
    shutdown: { timeoutMs: 2000 }, restart: { policy: 'always', minimumDelayMs: 500, maximumDelayMs: 2000 } };
  await atomicJson(join(source, 'services', componentId, 'deploy.json'), plan);
  await runCommand({ executable: 'git', args: ['init', '--quiet'], timeoutMs: 5000 }, source, config.executables, { jobLauncher: resolve('dist/native/ivy-job.exe') });
  const journal = new HostJournal(config), cleanups: (() => Promise<unknown>)[] = [];
  t.after(async () => { for (const cleanup of cleanups.reverse()) await cleanup(); journal.close(); assert.ok(relative(tmpdir(), root).startsWith('ivy-executor-test-')); await rm(root, { recursive: true, force: true }); });
  const prepare = async (label: string, ready = true, entrypoint = 'node') => {
    plan.readiness!.timeoutMs = ready ? 7000 : 2500; plan.entrypoint!.executable = entrypoint;
    await atomicJson(join(source, 'services', componentId, 'deploy.json'), plan);
    // Optional protocol integration uses this test distribution's actual SDK in the child. This
    // fixture is not the separate standalone SDK packaging acceptance (verify-sdk-package.mjs).
    const service = plan.connectsToHive ? `const {ServiceClient}=await import(${JSON.stringify(pathToFileURL(resolve('dist/packages/sdk/src/service.js')).href)});
service=new ServiceClient({publicBaseUrl:config.publicBaseUrl,credential:()=>config.credential,
identity:{serviceNodeId:config.serviceNodeId,hostId:config.hostId,serviceName:config.componentId,version:config.version,buildId:config.buildId,hiveProtocol:1},
registry:()=>({namespaces:[],contracts:[],requiredContracts:${JSON.stringify(plan.requirements.contracts)}}),handlers:{},reconcile:async()=>{},
readiness:async()=>({ready:${ready},diagnostics:[]}),onState:state=>{serviceState.ready=state.status==='ready';serviceState.generation=state.generation??null;},heartbeatMs:250});serviceState.ready=false;service.start();` : '';
    const script = `import fs from 'node:fs';import path from 'node:path';import crypto from 'node:crypto';import {replaceFile} from './atomic-file.mjs';
const config=JSON.parse(fs.readFileSync(process.env.IVY_INSTANCE_CONFIG,'utf8')),bootId=crypto.randomUUID(),startedAt=new Date().toISOString();
fs.mkdirSync(config.dataRoot,{recursive:true});fs.appendFileSync(path.join(config.dataRoot,'launches.txt'),${JSON.stringify(label)}+' '+bootId+'\\n');
fs.writeFileSync(path.join(config.dataRoot,'launch-settings.json'),JSON.stringify(config.settings));
const serviceState={ready:true,generation:null};let service=null;${service}
const tick=async()=>{const health={schemaVersion:1,instanceId:config.instanceId,componentId:config.componentId,buildId:config.buildId,pid:process.pid,bootId,launchId:process.env.IVY_LAUNCH_ID,startedAt,observedAt:new Date().toISOString(),ready:${ready}&&serviceState.ready,generation:serviceState.generation,details:''};
fs.writeFileSync(path.join(config.dataRoot,'health.json.tmp'),JSON.stringify(health));await replaceFile(path.join(config.dataRoot,'health.json.tmp'),path.join(config.dataRoot,'health.json'));
try{const control=JSON.parse(fs.readFileSync(path.join(config.dataRoot,'control.json'),'utf8'));if(control.launchId===health.launchId&&control.bootId===bootId){clearInterval(timer);await service?.stop();process.exitCode=0;}}catch{}};
let busy=false;const observe=()=>{if(busy)return;busy=true;void tick().catch(error=>{console.error(error);clearInterval(timer);process.exitCode=1;}).finally(()=>{busy=false;});};const timer=setInterval(observe,100);observe();`;
    await writeFile(join(source, 'packages/main.mjs'), script);
    const snapshot = await captureSource(source, config); return prepareCandidate(snapshot, componentId, config, resolve('.'));
  };
  return { root, source, plan, config, configPath, journal, prepare, cleanups };
}
