import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, unlink, writeFile, mkdir, rmdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, relative, resolve } from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { bootstrapOs } from '../packages/host-runtime/src/bootstrap-os.js';
import { linuxUnit } from '../packages/host-runtime/src/bootstrap.js';
import { digest } from '../packages/contracts/src/canonical.js';
import { atomicJson } from '../packages/host-runtime/src/config.js';
import type { Host } from '../packages/contracts/src/generated.js';
import { linuxResourceScope, linuxSlice, ensureLinuxSlice, inspectLinuxSlice } from '../packages/host-runtime/src/linux-resources.js';

test('real systemd replaces only its stopped fixture unit and rejects a third definition state', { skip: process.platform !== 'linux' || process.getuid?.() !== 0, timeout: 45_000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-bootstrap-os-test-')), installationId = 'ivy-next-' + digest(root).slice(7, 19);
  const plan: Host.BootstrapPlan = { schemaVersion: 1, hostId: 'disabled-systemd-fixture', os: 'linux', installationId, configHash: digest('fixture configuration'),
    candidateId: digest('old fixture'), artifactRoot: join(root, 'old'), configPath: join(root, 'host.json'), runtimeRoot: join(root, 'state'), nodeExecutable: process.execPath,
    processes: [{ instanceId: 'fixture', componentId: 'fixture', name: installationId + '-111111111111', artifactRoot: join(root, 'old') }] };
  const next = { ...plan, artifactRoot: join(root, 'new'), candidateId: digest('new fixture'),
    processes: [{ ...plan.processes[0]!, artifactRoot: join(root, 'new'), candidateId: digest('new process fixture') }] }, os = bootstrapOs(resolve('.'));
  const path = join('/etc/systemd/system', plan.processes[0]!.name + '.service'), initial = linuxUnit(plan, plan.processes[0]!), changed = linuxUnit(next, next.processes[0]!) + '# explicitly owned fixture alteration\n';
  let created = false;
  t.after(async () => {
    if (created) {
      assert.ok([initial, linuxUnit(next, next.processes[0]!), changed].includes(await readFile(path, 'utf8')), 'Fixture unit ownership changed; retain it for inspection.');
      await unlink(path); await os.reload();
    }
    assert.ok(relative(tmpdir(), root).startsWith('ivy-bootstrap-os-test-')); await rm(root, { recursive: true, force: true });
  });
  await writeFile(path, initial, { mode: 0o644, flag: 'wx' }); created = true; await os.reload();
  const owner = (await os.inspect([plan, next]))[0]!; assert.equal(owner.running, false); assert.equal(owner.enabled, false);
  const desired = await os.render(owner, next); await os.check(desired);
  await os.apply(owner, desired, [plan, next]); await os.reload(); await os.apply(owner, desired, [plan, next]);
  const after = (await os.inspect([plan, next]))[0]!;
  assert.equal(after.definitionHash, desired.hash); assert.equal(after.candidateId, next.processes[0]!.candidateId); assert.equal(after.running, false); assert.equal(after.enabled, false);
  await writeFile(path, changed); await os.reload();
  await assert.rejects(os.apply(owner, desired, [plan, next])); assert.equal(await readFile(path, 'utf8'), changed);
});

test('real systemd resource inspection refuses altered slice bytes and loaded limit overrides', { skip: process.platform !== 'linux' || process.getuid?.() !== 0, timeout: 30_000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-resource-os-test-')), installationId = 'ivy-next-' + digest(root).slice(7, 19);
  const scope = linuxResourceScope(installationId, { memoryHighBytes: 192 * 1024 * 1024, memoryMaxBytes: 256 * 1024 * 1024, tasksMax: 64 });
  const path = '/etc/systemd/system/' + scope.slice, overrideRoot = path + '.d', overridePath = join(overrideRoot, 'fixture.conf');
  const content = linuxSlice(scope), altered = content + '# deliberately changed owned fixture\n', override = '[Slice]\nMemoryMax=536870912\n';
  const os = bootstrapOs(resolve('.')); let created = false, overridden = false;
  t.after(async () => {
    if (overridden) { assert.equal(await readFile(overridePath, 'utf8'), override); await unlink(overridePath); await rmdir(overrideRoot); }
    if (created) { assert.ok([content, altered].includes(await readFile(path, 'utf8'))); await unlink(path); await os.reload(); }
    assert.ok(relative(tmpdir(), root).startsWith('ivy-resource-os-test-')); await rm(root, { recursive: true, force: true });
  });
  // The unique name must be absent before this fixture claims it.
  await assert.rejects(readFile(path), { code: 'ENOENT' });
  await ensureLinuxSlice(scope); created = true; await os.reload(); await inspectLinuxSlice(scope);
  await ensureLinuxSlice(scope);
  await writeFile(path, altered); await assert.rejects(ensureLinuxSlice(scope)); assert.equal(await readFile(path, 'utf8'), altered);
  await writeFile(path, content); await mkdir(overrideRoot); await writeFile(overridePath, override, { flag: 'wx', mode: 0o644 }); overridden = true;
  await os.reload(); await assert.rejects(inspectLinuxSlice(scope));
  await unlink(overridePath); await rmdir(overrideRoot); overridden = false; await os.reload(); await inspectLinuxSlice(scope);
});

test('real Windows scheduler replaces a disabled fixture definition without starting it or changing its other settings', { skip: process.platform !== 'win32', timeout: 45_000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-bootstrap-os-test-')), installationId = 'ivy-next-' + digest(root).slice(7, 19);
  const plan: Host.BootstrapPlan = { schemaVersion: 1, hostId: 'disabled-scheduler-fixture', os: 'win32', installationId, configHash: digest('fixture configuration'),
    candidateId: digest('old fixture'), artifactRoot: join(root, 'old'), configPath: join(root, 'host.json'), runtimeRoot: join(root, 'state'), nodeExecutable: process.execPath,
    processes: [{ instanceId: 'fixture', componentId: 'host-executor', name: installationId + '-111111111111', configPath: join(root, 'instance.json'),
      entrypoint: { executable: 'node', args: ['dist/services/fixture/src/main.js'], timeoutMs: 30_000 } }] };
  const next = { ...plan, artifactRoot: join(root, 'new'), candidateId: digest('new fixture') };
  const equivalent = { ...plan, configHash: digest('equivalent fixture configuration') };
  const scriptPath = join(root, 'scheduler-fixture.ps1'), dataPath = join(root, 'plans.json'); await atomicJson(dataPath, [plan, next]);
  const principalHelper = resolve('dist/packages/host-runtime/assets/windows-principal.ps1');
  await writeFile(scriptPath, `param([string]$DataPath,[string]$PrincipalHelper,[string]$Action)
$ErrorActionPreference='Stop'
. $PrincipalHelper
$plans=Get-Content -LiteralPath $DataPath -Raw | ConvertFrom-Json
$plan=$plans[0]; $name=$plan.processes[0].name
if ($name -notmatch '^ivy-next-[0-9a-f]{12}-111111111111$' -or $plan.hostId -ne 'disabled-scheduler-fixture') { throw 'Invalid fixture scope.' }
$scheduler=New-Object -ComObject Schedule.Service; $scheduler.Connect(); $folder=$scheduler.GetFolder('\\')
$sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value
function Quote-Argument([string]$Value) { '"' + ([regex]::Replace([regex]::Replace($Value, '(\\\\*)"', '$1$1\\"'), '(\\\\+)$', '$1$1')) + '"' }
if ($Action -eq 'create') {
  $existing=$null; try { $existing=$folder.GetTask($name) } catch { if ($_.Exception.HResult -ne -2147024894) { throw } }
  if ($null -ne $existing) { throw 'Fixture name already exists.' }
  $definition=$scheduler.NewTask(0)
  $definition.RegistrationInfo.Description='IvyNext immutable bootstrap '+$plan.candidateId+' host '+$plan.hostId
  $definition.Principal.UserId=$sid; $definition.Principal.LogonType=3; $definition.Principal.RunLevel=0
  $definition.Settings.Enabled=$false; $definition.Settings.Hidden=$true; $definition.Settings.ExecutionTimeLimit='PT123S'; $definition.Settings.MultipleInstances=2
  $trigger=$definition.Triggers.Create(9); $trigger.UserId=$sid
  $taskAction=$definition.Actions.Create(0); $taskAction.Path=Join-Path $plan.artifactRoot 'dist/native/ivy-host-job.exe'; $taskAction.WorkingDirectory=$plan.artifactRoot
$taskAction.Arguments=(@('--parent','0',$plan.nodeExecutable,'dist/services/fixture/src/main.js','--config',$plan.configPath,'--instance-config',$plan.processes[0].configPath) | ForEach-Object { Quote-Argument $_ }) -join ' '
  $null=$folder.RegisterTaskDefinition($name,$definition,2,$sid,$null,3,$null)
}
$task=$folder.GetTask($name); $definition=$task.Definition
if ($task.Enabled -or $task.State -eq 4 -or (Resolve-IvyPrincipalSid $definition.Principal.UserId) -ne $sid -or $definition.Actions.Count -ne 1 -or $definition.Actions.Item(1).WorkingDirectory -notin @($plans | ForEach-Object { $_.artifactRoot })) { throw 'Fixture ownership changed.' }
if ($Action -eq 'remove') { $folder.DeleteTask($name,0) } else { @{lastRun=$task.LastRunTime.ToString('o');enabled=$task.Enabled;limit=$definition.Settings.ExecutionTimeLimit;triggers=$definition.Triggers.Count;hidden=$definition.Settings.Hidden} | ConvertTo-Json -Compress }
`);
  const exec = promisify(execFile), shell = join(process.env['SystemRoot']!, 'System32/WindowsPowerShell/v1.0/powershell.exe');
  const call = (action: string) => exec(shell, ['-NoProfile', '-NonInteractive', '-File', scriptPath, '-DataPath', dataPath, '-PrincipalHelper', principalHelper, '-Action', action], { windowsHide: true, timeout: 10_000, maxBuffer: 4096 });
  let created = false;
  t.after(async () => {
    if (created) await call('remove');
    assert.ok(relative(tmpdir(), root).startsWith('ivy-bootstrap-os-test-')); await rm(root, { recursive: true, force: true });
  });
  const before = JSON.parse((await call('create')).stdout); created = true;
  const os = bootstrapOs(resolve('.')), owner = (await os.inspect([plan, equivalent, next]))[0]!;
  assert.equal(owner.componentId, 'host-executor'); assert.equal(owner.running, false); assert.equal(owner.enabled, false);
  const snapshot: Host.BootstrapSnapshot = { schemaVersion: 1, hostId: plan.hostId, installationId: plan.installationId,
    configPath: plan.configPath, configurationHash: plan.configHash, observedAt: new Date().toISOString(),
    owners: [{ instanceId: owner.instanceId, componentId: owner.componentId, name: owner.name, candidateId: owner.candidateId,
      definitionHash: owner.definitionHash, enabled: owner.enabled, running: owner.running }] };
  await os.pause!(snapshot, [plan, equivalent, next]); await os.resume!(snapshot, [plan, equivalent, next]);
  const desired = await os.render(owner, next); await os.check(desired);
  assert.notEqual(desired.hash, owner.definitionHash);
  await os.apply(owner, desired, [plan, equivalent, next]); await os.apply(owner, desired, [plan, equivalent, next]);
  const after = (await os.inspect([plan, equivalent, next]))[0]!;
  assert.equal(after.definitionHash, desired.hash); assert.equal(after.candidateId, next.candidateId); assert.equal(after.running, false); assert.equal(after.enabled, false);
  assert.deepEqual(JSON.parse((await call('status')).stdout), before);
  const altered = desired.content.replace(next.candidateId, digest('tampered candidate'));
  assert.notEqual(altered, desired.content); await assert.rejects(os.check({ ...desired, content: altered }));
});
