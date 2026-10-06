import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { linuxUnit, replaceableSystemdOwner } from '../packages/host-runtime/src/bootstrap.js';
import { digest } from '../packages/contracts/src/canonical.js';
import type { Host } from '../packages/contracts/src/generated.js';

test('actual systemd parser accepts generated unit paths and literal argv', { skip: process.platform !== 'linux', timeout: 15_000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-bootstrap-test-'));
  t.after(async () => { assert.ok(relative(tmpdir(), root).startsWith('ivy-bootstrap-test-')); await rm(root, { recursive: true, force: true }); });
  const artifactRoot = join(root, 'space % literal $directory'); await mkdir(artifactRoot);
  const plan: Host.BootstrapPlan = { schemaVersion: 1, os: 'linux', hostId: 'unit-parser-fixture', installationId: 'ivy-next-111111111111', candidateId: digest('synthetic candidate'), artifactRoot,
    configPath: join(root, 'private config.json'), configHash: digest('synthetic config'), runtimeRoot: join(root, 'state'), nodeExecutable: process.execPath,
    processes: [{ instanceId: 'fixture', componentId: 'fixture', name: 'ivy-next-111111111111-222222222222' }] };
  const path = join(root, plan.processes[0]!.name + '.service'); await writeFile(path, linuxUnit(plan, plan.processes[0]!));
  await promisify(execFile)('/usr/bin/systemd-analyze', ['verify', path], { timeout: 10_000, maxBuffer: 65536 });
});

test('bootstrap gives the host executor its independent instance configuration and preserves the declared restart policy', () => {
  const plan: Host.BootstrapPlan = { schemaVersion: 1, os: 'linux', hostId: 'bootstrap-contract-fixture', installationId: 'ivy-next-111111111111',
    candidateId: digest('bootstrap-candidate'), artifactRoot: '/srv/ivy/artifact', configPath: '/srv/ivy/host.json', configHash: digest('bootstrap-config'),
    runtimeRoot: '/srv/ivy/runtime', nodeExecutable: '/usr/bin/node', processes: [{ instanceId: 'executor', componentId: 'host-executor',
      name: 'ivy-next-111111111111-222222222222', configPath: '/srv/ivy/runtime/instances/executor.json',
      restart: { policy: 'never', minimumDelayMs: 100, maximumDelayMs: 1000 } }] };
  const content = linuxUnit(plan, plan.processes[0]!);
  assert.match(content, /Restart=no\n/);
  assert.ok(content.includes('"--config" "/srv/ivy/host.json" "--instance-config" "/srv/ivy/runtime/instances/executor.json"'));
  assert.ok(content.includes('Environment=IVY_INSTANCE_CONFIG="/srv/ivy/runtime/instances/executor.json"'));
  assert.equal(replaceableSystemdOwner(content), true);
  const legacy = '[Unit]\nDescription=IvyNext old\n[Service]\nExecStart=/usr/bin/node guardian.js\nEnvironment=IVY=1\n';
  assert.equal(replaceableSystemdOwner(legacy), false);
  assert.equal(replaceableSystemdOwner(legacy, true), true);
  assert.equal(replaceableSystemdOwner('[Unit]\nDescription=foreign\n'), false);
});

test('Linux AgentManager uses the non-administrative service identity',()=>{
  const plan:Host.BootstrapPlan={schemaVersion:1,os:'linux',hostId:'host',installationId:'ivy-next-111111111111',candidateId:digest('candidate'),artifactRoot:'/artifact',configPath:'/config.json',configHash:digest('config'),runtimeRoot:'/runtime',nodeExecutable:'/usr/bin/node',
    processes:[{instanceId:'agent-manager',componentId:'agent-manager',name:'ivy-next-111111111111-222222222222',user:'ivy-agent'}]};
  const unit=linuxUnit(plan,plan.processes[0]!);assert.match(unit,/\nUser=ivy-agent\nGroup=ivy-agent\n/);
});

test('Windows task principals normalize actual account names and SIDs without admitting another identity', { skip: process.platform !== 'win32', timeout: 15_000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-bootstrap-test-'));
  t.after(async () => { assert.ok(relative(tmpdir(), root).startsWith('ivy-bootstrap-test-')); await rm(root, { recursive: true, force: true }); });
  const path = join(root, 'principal.ps1');
  await writeFile(path, `param([string]$Helper)\n$ErrorActionPreference='Stop'\n. $Helper\n$identity=[System.Security.Principal.WindowsIdentity]::GetCurrent()\n@{expected=$identity.User.Value;fromName=(Resolve-IvyPrincipalSid $identity.Name);fromSid=(Resolve-IvyPrincipalSid $identity.User.Value);other=(Resolve-IvyPrincipalSid 'S-1-5-20')} | ConvertTo-Json -Compress\ntry { Resolve-IvyPrincipalSid '' | Out-Null; exit 2 } catch {}\ntry { Resolve-IvyPrincipalSid 'IvyMissingPrincipal-test-invalid' | Out-Null; exit 3 } catch {}\n`);
  const result = await promisify(execFile)(join(process.env['SystemRoot']!, 'System32/WindowsPowerShell/v1.0/powershell.exe'),
    ['-NoProfile', '-NonInteractive', '-File', path, '-Helper', resolve('packages/host-runtime/assets/windows-principal.ps1')], { windowsHide: true, timeout: 10_000, maxBuffer: 4096 });
  const value = JSON.parse(result.stdout) as Record<string, string>;
  assert.equal(value['fromName'], value['expected']); assert.equal(value['fromSid'], value['expected']); assert.notEqual(value['other'], value['expected']);
});
