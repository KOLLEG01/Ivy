import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { cpSync, mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { hostname } from 'node:os';
import { dirname, isAbsolute, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { setTimeout as delay } from 'node:timers/promises';
import { artifactFiles, fileHash } from '../../../dist/packages/host-runtime/src/artifact.js';
import { runtimeEnvironment } from '../../../dist/packages/host-runtime/src/process.js';
import { PhoneJournal } from '../../../dist/services/phone-bridge/src/runtime/journal.js';
import { startPhoneProcess } from '../../../dist/services/phone-bridge/src/runtime/process.js';

// Actual published EXE through the production job/client/journal, never a synthetic native child.
// Local configuration only: no INVITE, account registration, devices, Desktop, Voice or user data.
const { values } = parseArgs({ options: { output: { type: 'string' } } });
assert.equal(process.platform, 'win32'); assert.ok(values.output && isAbsolute(values.output));
const root = fileURLToPath(new URL('../../..', import.meta.url)), work = join(root, '.local/phone-package-' + randomUUID());
const artifactRoot = join(work, 'artifact'), dataRoot = join(work, 'data'), executable = 'dist/native/phone/Ivy.PhoneRuntime.exe';
mkdirSync(join(artifactRoot, 'dist/native'), { recursive: true });
const originalFiles = await artifactFiles(join(root, 'dist/native/phone'));
const evs = JSON.parse(readFileSync(join(root, 'dist/native/phone/evs-build.json'), 'utf8'));
assert.equal(evs.abi, 1);
assert.equal(await fileHash(join(root, 'dist/native/phone/ivy_phone_evs.dll')), 'sha256:' + evs.librarySha256);
assert.ok(readFileSync(join(root, 'dist/native/phone/licenses/EVS-reference.md')).length > 100);
cpSync(join(root, 'dist/native/phone'), join(artifactRoot, 'dist/native/phone'), { recursive: true, errorOnExist: true, force: false });
cpSync(join(root, 'dist/native/ivy-job.exe'), join(artifactRoot, 'dist/native/ivy-job.exe'), { errorOnExist: true, force: false });
assert.deepEqual(await artifactFiles(join(artifactRoot, 'dist/native/phone')), originalFiles);
assert.ok(!Object.keys(runtimeEnvironment()).some(key => key.toUpperCase().startsWith('DOTNET_')), 'Production environment must not depend on developer runtime overrides.');
const started = performance.now(), report = { source: JSON.parse(readFileSync(join(root, 'dist/build-info.json'), 'utf8')), host: hostname(),
  phase: 'not_proven', work, fileCount: originalFiles.length, packageBytes: originalFiles.reduce((sum, file) => sum + file.bytes, 0),
  executableHash: await fileHash(join(artifactRoot, executable)), normalExit: null, missingRuntimeRejected: false,
  intents: 0, abruptOwnerRecovered: false, unknownPrepareRetained: false, evsPrepared: false,
  evsLibraryHash: 'sha256:' + evs.librarySha256, realVoiceOrAudio: false, error: null, elapsedMs: null };
let journal = new PhoneJournal(dataRoot, { hostId: 'phone-package-fixture', serviceNodeId: 'phone-package-fixture' });
let processOwner;
const options = { artifactRoot, executable, executableHash: report.executableHash, journal };
try {
  await assert.rejects(startPhoneProcess({ ...options, executableHash: 'sha256:' + '0'.repeat(64) }), { code: 'phone_build_mismatch' });
  assert.equal(journal.epoch, null, 'Artifact mismatch must precede native epoch admission.');
  processOwner = await startPhoneProcess(options);
  const send = async (method, args) => {
    const operationId = randomUUID(), response = await processOwner.client.request(method, args, operationId);
    assert.equal(response.ok, true); assert.equal(journal.get(operationId).phase, 'result'); return response;
  };
  await send('configure', { binding: { address: '127.0.0.1', port: 0, transport: 'udp' },
    codecs: { preferences: ['EVS', 'PCMA'], packetMs: 20 }, incomingPeers: [], registration: null, password: null });
  const callId = randomUUID(); await send('call.prepare', { callId });
  report.evsPrepared = true; // Preparation initializes both packaged ABI directions without audio devices or a SIP INVITE.
  const status = await processOwner.client.request('status', {}, null);
  assert.equal(status.result.call.id, callId); assert.deepEqual(status.result.media.audio, { callId, state: 'unprepared', route: null });
  await send('call.hangup', { callId }); await send('call.release', { callId });
  await processOwner.close(); const completion = await processOwner.completion; processOwner = null;
  assert.equal(completion.errorCode, null); assert.equal(completion.exitCode, 0); assert.equal(journal.epoch, null);
  report.normalExit = completion.exitCode; report.intents = journal.status().operations; assert.equal(report.intents, 4);

  // Kill only this synthetic service parent after the actual native prepare receipt arrives,
  // before its journal commit. The Windows job/native lease and the production startup path
  // must settle ownership; neither the prepare nor an external call may be replayed.
  journal.close();
  const crashScript = join(work, 'crash-owner.mjs'), crashMarker = join(work, 'crash-owner.json');
  writeFileSync(crashScript, `import {PhoneJournal} from ${JSON.stringify(new URL('../../../dist/services/phone-bridge/src/runtime/journal.js', import.meta.url).href)};
import {startPhoneProcess} from ${JSON.stringify(new URL('../../../dist/services/phone-bridge/src/runtime/process.js', import.meta.url).href)};
import {writeFileSync} from 'node:fs'; import {randomUUID} from 'node:crypto';
const journal = new PhoneJournal(${JSON.stringify(dataRoot)}, {hostId:'phone-package-fixture', serviceNodeId:'phone-package-fixture'});
const originalResolve = journal.hooks.beforeResolve;
journal.hooks.beforeResolve = async (intent, reply) => {
  if (intent.method === 'call.prepare') {
    if (!reply.ok) throw new Error('Actual native prepare failed.');
    writeFileSync(${JSON.stringify(crashMarker)}, JSON.stringify(intent), {flag:'wx'});
    process.exit(23);
  }
  await originalResolve(intent, reply);
};
const owner = await startPhoneProcess({artifactRoot:${JSON.stringify(artifactRoot)}, executable:${JSON.stringify(executable)}, executableHash:${JSON.stringify(report.executableHash)}, journal});
await owner.client.request('configure', {binding:{address:'127.0.0.1',port:0,transport:'udp'},codecs:{preferences:['PCMA'],packetMs:20},incomingPeers:[],registration:null,password:null}, randomUUID());
await owner.client.request('call.prepare', {callId:randomUUID()}, randomUUID());
throw new Error('Crash boundary was not reached.');
`, { flag: 'wx' });
  const crashed = spawn(process.execPath, [crashScript], { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'], env: runtimeEnvironment() });
  let crashError = ''; crashed.stdout.resume(); crashed.stderr.on('data', value => { crashError = (crashError + String(value)).slice(-4096); });
  let deadline;
  try {
    const code = await Promise.race([
      new Promise((resolve, reject) => { crashed.once('error', reject); crashed.once('close', resolve); }),
      new Promise((_, reject) => { deadline = setTimeout(() => { crashed.kill(); reject(new Error('Fixture owner did not reach its crash boundary.')); }, 15000); }),
    ]);
    assert.equal(code, 23, crashError);
  } finally { clearTimeout(deadline); }
  journal = new PhoneJournal(dataRoot, { hostId: 'phone-package-fixture', serviceNodeId: 'phone-package-fixture' }); options.journal = journal;
  const unknown = JSON.parse(readFileSync(crashMarker, 'utf8'));
  assert.equal(journal.epoch, unknown.epoch); assert.equal(journal.get(unknown.operationId).phase, 'submitted');
  const recoveryDeadline = performance.now() + 10000;
  while (!processOwner) {
    try { processOwner = await startPhoneProcess(options); }
    catch (error) {
      assert.equal(journal.epoch, unknown.epoch, 'A failed replacement cannot retire the original journal epoch.');
      if (performance.now() >= recoveryDeadline) throw error;
      await delay(100); // Await actual original process termination; production never auto-retries.
    }
  }
  assert.notEqual(processOwner.epoch, unknown.epoch); assert.equal(journal.get(unknown.operationId).phase, 'outcome_unknown');
  assert.equal((await processOwner.client.observe()).call, null);
  assert.throws(() => journal.submit(unknown), { code: 'phone_operation_retained' });
  assert.equal(journal.status().operations, 6, 'Recovery must not issue replacement configuration or call commands.');
  await processOwner.close(); processOwner = null;
  assert.equal(journal.epoch, null); report.abruptOwnerRecovered = true; report.unknownPrepareRetained = true; report.intents = 6;
  const runtime = join(artifactRoot, 'dist/native/phone/coreclr.dll');
  assert.ok(relative(work, runtime).startsWith('artifact'), 'Only the isolated package copy may be damaged.');
  renameSync(runtime, runtime + '.disabled');
  await assert.rejects(startPhoneProcess(options));
  assert.equal(journal.epoch, null, 'Failed native startup must reconcile its confirmed process exit.');
  assert.equal(journal.status().operations, 6, 'Broken package cannot admit a new SIP command.');
  report.missingRuntimeRejected = true;
  assert.deepEqual(await artifactFiles(join(root, 'dist/native/phone')), originalFiles, 'The original package must remain intact.');
  report.phase = 'passed';
} catch (error) { report.error = error.code ?? error.message; throw error; }
finally {
  try { if (processOwner) await processOwner.close(); }
  finally {
    journal.close(); report.elapsedMs = Math.round(performance.now() - started);
    mkdirSync(dirname(values.output), { recursive: true }); writeFileSync(values.output, JSON.stringify(report, null, 2) + '\n', { flag: 'wx' });
  }
}
process.stdout.write('phone_native_package_passed: published EXE through Windows job, real client/journal, original cleanup, abrupt service-owner recovery with unknown prepare retained, and missing-runtime refusal; no device/Voice/carrier\n');
