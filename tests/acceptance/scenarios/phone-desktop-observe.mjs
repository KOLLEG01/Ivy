import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { lstat, mkdir, open, readFile, realpath } from 'node:fs/promises';
import { hostname } from 'node:os';
import { basename, dirname, isAbsolute, join, resolve } from 'node:path';
import { parseArgs, promisify } from 'node:util';
import { Ajv2020 } from 'ajv/dist/2020.js';

// Read-only target check: no launch, input, focus, audio capture or native task operation.
const { values } = parseArgs({ options: { assembly: { type: 'string' }, 'assembly-hash': { type: 'string' },
  'application-id': { type: 'string' }, 'expected-host': { type: 'string' }, output: { type: 'string' },
  'process-ownership': { type: 'boolean', default: false } } });
for (const key of ['assembly', 'assembly-hash', 'application-id', 'expected-host', 'output']) assert.ok(values[key], 'Missing --' + key);
assert.equal(process.platform, 'win32'); assert.equal(hostname(), values['expected-host']);
assert.match(values['assembly-hash'], /^sha256:[0-9a-f]{64}$/);
assert.match(values['application-id'], /^[A-Za-z0-9_.-]+![A-Za-z0-9_.-]+$/); assert.ok(values['application-id'].length <= 128);
assert.ok(isAbsolute(values.assembly)); assert.ok(isAbsolute(values.output));
const assembly = await realpath(values.assembly), metadata = await lstat(assembly);
assert.ok(metadata.isFile() && metadata.size <= 2 * 1024 * 1024);
const assemblyHash = 'sha256:' + createHash('sha256').update(await readFile(assembly)).digest('hex');
assert.equal(assemblyHash, values['assembly-hash']);
const schema = JSON.parse(await readFile(new URL('../../../specs/schemas/phone-launch.schema.json', import.meta.url), 'utf8'));
const valid = new Ajv2020({ strict: false }).compile({ ...schema, $ref: '#/$defs/DesktopObservation' });
const captureSchema = JSON.parse(await readFile(new URL('../../../specs/schemas/phone-capture.schema.json', import.meta.url), 'utf8'));
const validProcess = new Ajv2020({ strict: false }).compile({ ...captureSchema, $ref: '#/$defs/ProcessOwnership' });
const output = resolve(values.output); await mkdir(dirname(output), { recursive: true });
const handle = await open(output, 'wx', 0o600), started = performance.now();
const report = { schemaVersion: 1, host: hostname(), node: process.version, startedAt: new Date().toISOString(),
  assembly, assemblyHash, appUserModelId: values['application-id'], phase: 'not_proven', observation: null,
  ownerCurrent: false, processOwnership: null, readOnly: true, voiceOrAudioProven: false, error: null, elapsedMs: null };
try {
  const powershell = join(process.env.SystemRoot ?? 'C:\\Windows', 'System32/WindowsPowerShell/v1.0/powershell.exe');
  const script = `
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
[void][System.Reflection.Assembly]::LoadFrom($env:IVY_PHONE_OBSERVER_ASSEMBLY)
$phoneApplication = New-Object Ivy.PhoneBridge.WindowsDesktopApplication -ArgumentList @($env:IVY_PHONE_OBSERVER_APPLICATION)
$phoneObservation = $phoneApplication.Observe()
$phoneOwnerCurrent = $false
if ($null -ne $phoneObservation.identity) { $phoneOwnerCurrent = $phoneObservation.identity.Owner().IsCurrent() }
$phoneProcessOwnership = $null
if ($env:IVY_PHONE_PROCESS_OWNERSHIP -eq '1' -and $null -ne $phoneObservation.identity) {
  $phoneProcessOwnership = @{
    root = [Ivy.PhoneBridge.WindowsAudioProcessOwnership]::Observe($phoneObservation.identity, $phoneObservation.identity.pid)
    foreign = [Ivy.PhoneBridge.WindowsAudioProcessOwnership]::Observe($phoneObservation.identity, [int]$env:IVY_PHONE_OBSERVER_PID)
  }
}
@{ observation = $phoneObservation; ownerCurrent = $phoneOwnerCurrent; processOwnership = $phoneProcessOwnership } | ConvertTo-Json -Depth 6 -Compress
`;
  const result = await promisify(execFile)(powershell, ['-NoLogo', '-NoProfile', '-NonInteractive', '-Command', script], {
    windowsHide: true, timeout: 15_000, maxBuffer: 64 * 1024, encoding: 'utf8',
    env: { ...process.env, IVY_PHONE_OBSERVER_ASSEMBLY: assembly, IVY_PHONE_OBSERVER_APPLICATION: values['application-id'],
      IVY_PHONE_PROCESS_OWNERSHIP: values['process-ownership'] ? '1' : '0', IVY_PHONE_OBSERVER_PID: String(process.pid) }
  });
  const observed = JSON.parse(result.stdout.trim());
  assert.ok(valid(observed.observation), 'Native observation must match its complete contract.');
  report.observation = observed.observation; report.ownerCurrent = observed.ownerCurrent === true;
  assert.equal(report.observation.state, 'ready', 'The current Desktop must have one exact visible-window owner.');
  assert.equal(report.observation.identity.appUserModelId, values['application-id']);
  assert.equal(basename(report.observation.identity.imagePath).toLowerCase(), 'chatgpt.exe');
  assert.equal(report.ownerCurrent, true, 'The originally observed process must still be current.');
  if (values['process-ownership']) {
    assert.ok(validProcess(observed.processOwnership?.root) && validProcess(observed.processOwnership?.foreign), 'Process observations must match their contract.');
    report.processOwnership = observed.processOwnership;
    assert.equal(report.processOwnership.root.pid, report.observation.identity.pid);
    assert.equal(report.processOwnership.root.state, 'owned');
    assert.equal(report.processOwnership.foreign.pid, process.pid);
    assert.equal(report.processOwnership.foreign.state, 'foreign');
  }
  report.phase = 'desktop_owner_observed';
} catch (error) {
  // Do not retain helper stdout/stderr or environment values as an exception dump.
  report.error = { code: typeof error.code === 'string' ? error.code : 'observation_failed',
    message: error instanceof assert.AssertionError ? error.message : 'Read-only Desktop observation failed.' };
  process.exitCode = 1;
} finally {
  report.elapsedMs = Math.round(performance.now() - started);
  try { await handle.writeFile(JSON.stringify(report, null, 2) + '\n'); await handle.sync(); } finally { await handle.close(); }
}
console.log(JSON.stringify({ phase: report.phase, output, readOnly: true, elapsedMs: report.elapsedMs }));
