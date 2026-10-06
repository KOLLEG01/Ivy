import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

test('profile CLI preserves source and enclosing Hive fields and refuses overwrites', async t => {
  const root = await mkdtemp(join(tmpdir(), 'phone-profile-')); t.after(() => rm(root, { recursive: true, force: true }));
  const input = join(root, 'input.json'), output = join(root, 'output.json');
  const config = { credential: 'synthetic-credential-never-print', serviceNodeId: 'phone-fixture', settings: {
    native: { executable: 'dist/native/phone/Ivy.PhoneRuntime.exe', executableHash: 'sha256:' + '0'.repeat(64) },
    binding: { address: '127.0.0.1', port: 0, transport: 'udp' }, codecs: { preferences: ['G722'], packetMs: 20 },
    registration: null, credentialsPath: null, policy: { recipients: [], incoming: [] },
    incomingPrincipalId: null, incomingRoute: 'windows', outgoingRoute: 'windows', application: { appUserModelId: 'Fixture.Unused!UI', startIfMissing: false },
    audio: { captureEndpointId: 'capture', renderEndpointId: 'render', sourceMode: 'desktop_process', captureBufferMs: 20, renderLatencyMs: 20, queueMs: 40 },
    voiceArchive: null, voiceInput: { micro: { usbipExecutable: 'C:\\verified\\usbip.exe', executableHash: 'sha256:' + '0'.repeat(64), port: 3241 }, hotkeyFallback: null },
    ringSeconds: 30, pollMs: 500,
  } };
  const original = JSON.stringify(config); await writeFile(input, original);
  const run = target => spawnSync(process.execPath, ['tools/testing/phone-audio-test-profile.mjs', input, target, 'EvsPreferred', 'Conservative'], { encoding: 'utf8' });
  const completed = run(output); assert.equal(completed.status, 0, completed.stderr);
  assert.equal(completed.stdout.includes(config.credential), false);
  const prepared = JSON.parse(await readFile(output, 'utf8'));
  assert.equal(prepared.credential, config.credential); assert.equal(prepared.serviceNodeId, config.serviceNodeId);
  assert.deepEqual(prepared.settings.codecs.preferences, ['EVS', 'G722', 'PCMA', 'PCMU']);
  assert.equal(prepared.settings.audio.playbackPrebufferMs, 40);
  assert.equal(prepared.settings.audio.enableWasapiLowLatency, false);
  assert.equal(await readFile(input, 'utf8'), original);
  const retained = await readFile(output, 'utf8');
  assert.notEqual(run(output).status, 0); assert.equal(await readFile(output, 'utf8'), retained);
  assert.notEqual(run(input).status, 0); assert.equal(await readFile(input, 'utf8'), original);
});
