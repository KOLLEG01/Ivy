import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { join, resolve, relative } from 'node:path';
import { tmpdir } from 'node:os';
import { createServer } from 'node:net';
import { randomUUID } from 'node:crypto';
import { digest } from '../packages/contracts/src/canonical.js';
import { HiveServer } from '../services/hive/src/server.js';
import { HiveClient, serviceTools } from '../packages/sdk/src/client.js';
import { atomicJson } from '../packages/host-runtime/src/config.js';
import { fileHash } from '../packages/host-runtime/src/artifact.js';
import { startPhoneBridge } from '../services/phone-bridge/src/main.js';
import { until } from './fixtures/host.js';

// Explicit installed-package test. Synthetic speech and local SIP only. Invalid endpoint IDs
// deliberately test bridge failure before any Windows audio stream can be opened.
test('Hive screening preserves real SIP/RTP decisions and cleans bridge failure and synthesis cancellation', { timeout: 120000 }, async t => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-screening-service-')), artifactRoot = resolve('.');
  const peer = spawn('dotnet', [resolve('tests/native-phone/bin/Debug/net10.0-windows/Ivy.PhoneRuntime.Tests.dll'), '--screening-peer-fixture'],
    { windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
  const events: { state: string; port?: number; samples?: number; energy?: number; mode?: string }[] = [];
  let peerErrors = '';
  peer.stderr.on('data', bytes => { peerErrors += String(bytes).slice(0, 4096); });
  createInterface({ input: peer.stdout }).on('line', line => events.push(JSON.parse(line)));
  const peerExit = new Promise<void>((done, reject) => {
    peer.once('error', reject); peer.once('exit', code => code === 0 ? done() : reject(new Error('SIP fixture failed: ' + peerErrors)));
  });
  void peerExit.catch(() => undefined);
  let running: Awaited<ReturnType<typeof startPhoneBridge>> | null = null, hive: HiveServer | null = null;
  t.after(async () => {
    try { await running?.close(); }
    finally {
      await hive?.close(); peer.stdin.end();
      try { await Promise.race([peerExit, new Promise<never>((_, reject) => { const timer = setTimeout(() => reject(new Error('Peer exit timeout')), 5000); timer.unref(); })]); }
      finally { if (peer.exitCode === null) peer.kill(); assert.ok(relative(tmpdir(), root).startsWith('ivy-screening-service-')); await rm(root, { recursive: true, force: true }); }
    }
  });
  await until(() => events.some(event => event.state === 'ready'), 15000);
  const peerPort = events.find(event => event.state === 'ready')!.port!;
  const socket = createServer(); await new Promise<void>(done => socket.listen(0, '127.0.0.1', done));
  const port = (socket.address() as { port: number }).port; await new Promise<void>(done => socket.close(() => done()));
  const base = `http://127.0.0.1:${port}/ivy`, build = JSON.parse(await readFile('dist/build-info.json', 'utf8'));
  hive = new HiveServer({ filename: join(root, 'hive.sqlite'), publicBaseUrl: base, listenHost: '127.0.0.1', listenPort: port,
    version: 'fixture', buildId: digest('screening-hive'), credentials: ['admin', 'phone'].map(principalId => ({ principalId, digest: digest(principalId + '-token') })) });
  await hive.start();
  const credentialsPath = join(root, 'speech.json'); await atomicJson(credentialsPath, {schemaVersion:1,speech:{apiKey:'synthetic-only'}});
  const executable = 'dist/native/phone/Ivy.PhoneRuntime.exe';
  const config = { schemaVersion: 1, hostId: 'fixture', instanceId: 'phone', serviceNodeId: 'phone', componentId: 'phone-bridge',
    publicBaseUrl: base, dataRoot: join(root, 'phone'), artifactRoot, version: build.version, buildId: build.buildId, credential: 'phone-token',
    settings: { native: { executable, executableHash: await fileHash(resolve(executable)) },
      binding: { address: '127.0.0.1', port: 0, transport: 'udp' }, codecs: { preferences: ['PCMA'], packetMs: 20 }, registration: null, credentialsPath: null,
      policy: { recipients: [{ id: 'peer', destination: `sip:fixture@127.0.0.1:${peerPort}` }], incoming: [] },
      incomingPrincipalId: null, application: { appUserModelId: 'Fixture.Unused!UI', startIfMissing: false },
      audio: { captureEndpointId: 'nonexistent-fixture-capture', renderEndpointId: 'nonexistent-fixture-render', sourceMode: 'desktop_process', captureBufferMs: 20, renderLatencyMs: 20, queueMs: 40 },
      incomingRoute: 'windows', outgoingRoute: 'windows', voiceArchive: null, voiceInput: { micro: { usbipExecutable: 'C:\\fixture\\usbip.exe', executableHash: 'sha256:' + '0'.repeat(64), port: 3241 }, hotkeyFallback: null },
      ringSeconds: 10, pollMs: 250, speech: { credentialsPath, endpoint: 'https://speech.fixture.invalid/v1/audio/speech', model: 'fixture', voice: 'fixture' },
    } };
  const wav = Buffer.alloc(48044); wav.write('RIFF'); wav.writeUInt32LE(wav.length - 8, 4); wav.write('WAVEfmt ', 8);
  wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22); wav.writeUInt32LE(24000, 24); wav.writeUInt32LE(48000, 28);
  wav.writeUInt16LE(2, 32); wav.writeUInt16LE(16, 34); wav.write('data', 36); wav.writeUInt32LE(48000, 40);
  for (let i = 0; i < 24000; i++) wav.writeInt16LE(Math.round(8000 * Math.sin(2 * Math.PI * 440 * i / 24000)), 44 + i * 2);
  const realFetch = globalThis.fetch; let synthesis = 0, synthesisGate: Promise<void> | null = null;
  t.mock.method(globalThis, 'fetch', async (input: Parameters<typeof fetch>[0], init?: Parameters<typeof fetch>[1]) => {
    if (String(input) !== config.settings.speech.endpoint) return realFetch(input, init);
    synthesis++; if (synthesisGate) await synthesisGate; return new Response(wav);
  });
  const path = join(root, 'phone.json'); await atomicJson(path, config);
  running = await startPhoneBridge(path); await running.service.waitReady();
  const tools = serviceTools(new HiveClient(base, { credential: 'admin-token' }), 'phone', [{ namespace: 'phone', interfaceVersion: '1.0.0' }]);
  const screen = () => tools.call('phone.screen', { operationId: randomUUID(), recipientId: 'peer', announcement: 'Automated fixture call', timeoutSeconds: 2, repeatCount: 1 }, randomUUID()) as Promise<{ callId: string }>;
  const decision = async (callId: string, expected: string) => {
    let found = false;
    for (let i = 0; i < 60 && !found; i++) {
      const view = await tools.call('phone.call', { callId }) as { observation: { features?: { screening: string } } | null };
      found = view.observation?.features?.screening === expected;
      if (!found) await new Promise(done => setTimeout(done, 100));
    }
    assert.ok(found, 'Expected real screening decision: ' + expected);
  };
  const accepted = await screen(); await decision(accepted.callId, 'accepted');
  await assert.rejects(tools.call('phone.bridgeScreening', { callId: accepted.callId }, randomUUID()));
  await until(() => !running!.journal.currentCall(accepted.callId), 10000);
  await until(() => events.some(event => event.state === 'ended'), 10000);
  assert.ok(events.find(event => event.state === 'ended')!.energy! > 1, 'Peer receives the actual synthesized RTP announcement.');
  assert.equal(running.journal.callCommand(accepted.callId, 'call.screening.bridge')?.receipt?.ok, false);
  assert.equal(running.journal.callCommand(accepted.callId, 'call.release')?.receipt?.ok, true);
  for (const [mode, expected] of [['decline', 'declined'], ['silent', 'timeout']] as const) {
    peer.stdin.write(mode + '\n'); await until(() => events.some(event => event.state === 'mode' && event.mode === mode), 5000);
    const endings = events.filter(event => event.state === 'ended').length;
    const original = await screen(); await until(() => !running!.journal.currentCall(original.callId), 15000);
    await until(() => events.filter(event => event.state === 'ended').length > endings, 5000);
    const hangup = running.journal.callCommand(original.callId, 'call.hangup')?.receipt?.result as { features: { screening: string } };
    assert.equal(hangup.features.screening, expected);
    assert.equal(running.journal.callCommand(original.callId, 'call.screening.bridge'), null);
    assert.equal(running.journal.callCommand(original.callId, 'call.desktop.startVoice'), null);
  }
  const connectedBefore = events.filter(event => event.state === 'connected').length;
  let releaseSynthesis!: () => void; synthesisGate = new Promise<void>(done => { releaseSynthesis = done; });
  const cancelled = await screen(); await until(() => synthesis === 4, 10000);
  try { await tools.call('phone.hangup', { callId: cancelled.callId }, randomUUID()); }
  finally { releaseSynthesis(); }
  await until(() => !!running!.journal.callCommand(cancelled.callId, 'call.screening.synthesize')?.receipt, 10000);
  assert.equal(running.journal.currentCall(cancelled.callId), null);
  assert.equal(running.journal.callCommand(cancelled.callId, 'call.dial'), null);
  assert.equal(events.filter(event => event.state === 'connected').length, connectedBefore);
  assert.equal(running.journal.callCommand(accepted.callId, 'call.desktop.startVoice'), null);
  assert.ok(!events.some(event => event.state === 'failed'));
});
