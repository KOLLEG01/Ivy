import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, relative } from 'node:path';
import { PassThrough } from 'node:stream';
import { PhoneAdmission } from '../services/phone-bridge/src/runtime/admission.js';
import { PhoneCallCommands } from '../services/phone-bridge/src/runtime/calls.js';
import { PhoneFlow } from '../services/phone-bridge/src/runtime/flow.js';
import { PhoneAppToolsPool } from '../services/phone-bridge/src/runtime/desktop-app-tools-pool.js';
import type { PhoneFlowSettings } from '../services/phone-bridge/src/runtime/flow.js';
import type { PhoneVoiceArchive } from '../services/phone-bridge/src/runtime/desktop-voice-archive.js';
import type { PhoneVoiceTasks, PhoneVoiceSelectionCommand } from '../services/phone-bridge/src/runtime/desktop-voice-tasks.js';
import { PhoneJournal, phoneOutcomeReservation } from '../services/phone-bridge/src/runtime/journal.js';
import { PhoneNativeClient } from '../services/phone-bridge/src/runtime/native.js';
import type { PhoneIntent } from '../services/phone-bridge/src/runtime/native.js';
import { PhoneBridge } from '../services/phone-bridge/src/runtime/bridge.js';
import { phoneRegistry, validatePhoneService } from '../services/phone-bridge/src/runtime/registry.js';
import { phoneSettings } from '../services/phone-bridge/src/runtime/settings.js';
import { validateShared } from '../packages/contracts/src/validation.js';
import type { InvocationContext } from '../packages/sdk/src/service.js';
import { canonical, digest, IvyError } from '../packages/sdk/src/node.js';

const desktop = { pid: 123, startTimeUtcTicks: '1', imagePath: 'C:\\fixture.exe', appUserModelId: 'Fixture.Package!UI' };
const appTools = { nodeExecutable: process.execPath, nodeExecutableHash: 'sha256:' + '0'.repeat(64), serverPath: 'C:\\fixture\\server.mjs',
  serverHash: 'sha256:' + '0'.repeat(64), pipePath: 'auto', actorThreadId: '00000000-0000-4000-8000-000000000099' };
const settings = { outgoingRoute: 'voice' as const, voiceArchive: { databasePath: 'C:\\fixture\\state.sqlite', appTools }, application: { appUserModelId: desktop.appUserModelId, startIfMissing: false },
  audio: { captureEndpointId: 'capture', renderEndpointId: 'render', sourceMode: 'desktop_process', captureBufferMs: 20, renderLatencyMs: 20, queueMs: 40 },
  voiceInput: { micro: { usbipExecutable: 'C:\\verified\\usbip.exe', executableHash: 'sha256:' + '0'.repeat(64), port: 3241 }, hotkeyFallback: { modifiers: ['control', 'shift'], keyCode: 32, focusBeforeHotkey: true } }, ringSeconds: 30 };
const dispatch = { phase: 'submitted', method: 'hotkey',
  micro: { phase: 'submitted', press: { phase: 'submitted', pressed: true, generation: 4294967296 },
    release: { phase: 'submitted', pressed: false, generation: 4294967296 } },
  hotkey: { phase: 'submitted', requested: 6, submitted: 6, keyUpSubmitted: true, errorCode: null } };
const tick = () => new Promise<void>(done => setImmediate(done));
async function until(condition: () => boolean) {
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) { if (condition()) return; await new Promise(done => setTimeout(done, 5)); }
  assert.fail('Expected original flow did not settle.');
}
function deferred() { let resolve!: () => void; const promise = new Promise<void>(done => { resolve = done; }); return { promise, resolve }; }
type Request = { method: string; requestId: number; params: Record<string, unknown> };
function fixture(t: TestContext, maxBytes = 512 * 1024 * 1024, maxConcurrentCalls = 1,
  policyDefinition?: ConstructorParameters<typeof PhoneAdmission>[0]) {
  const root = mkdtempSync(join(tmpdir(), 'ivy-phone-flow-'));
  const policy = new PhoneAdmission(policyDefinition ?? {
    recipients: [{ id: 'personal', destination: 'sip:personal@127.0.0.1' }],
    incoming: [{ peerAddress: '127.0.0.1', transport: 'udp', fromUri: 'sip:known@fixture' }] });
  const journal = new PhoneJournal(root, { hostId: 'fixture', serviceNodeId: 'phone' }, { maxOperations: 100, maxBytes, maxEpochs: 100 }, maxConcurrentCalls);
  const epoch = randomUUID(); journal.beginEpoch(epoch);
  const input = new PassThrough(), output = new PassThrough(), requests: Request[] = [], errors: string[] = [];
  const native = new PhoneNativeClient(epoch, input, output, journal.hooks), calls = new PhoneCallCommands(policy, journal, native);
  let activeId: string | null = null, direction = 'outgoing', state = 'prepared', incoming: Record<string, unknown> | null = null;
  let desktopWaitingReads = 0;
  let intercept: ((request: Request) => Promise<void>) | null = null;
  let startState = 'active', stopState = 'stopped', launchPhase = 'ready', credentialReads = 0;
  let captureState: 'none' | 'matched' | 'ambiguous' | 'unavailable' = 'none';
  let registration: Record<string, unknown> | null = null;
  let screening = 'none', bridgeFailure = false, audioState = 'suspended';
  let command: string | null = null, commandSequence = 0, commandModel: string | null = null, commandEffort: string | null = null;
  const commands: { command: 'new_voice' | 'select_voice'; sequence: number; model: string | null; reasoningEffort: string | null }[] = [];
  const observation = () => ({ id: activeId, direction, state, sipCallId: incoming ? 'wire' : null, error: null, incoming,
    features: { access: 'trusted', disableCodecUpgrade: false, screening, command, commandSequence, commandState: command ? 'pending' : null,
      model: commandModel, reasoningEffort: commandEffort, commands, commandOverflow: false } });
  const respond = async (r: Request) => {
    if (intercept) await intercept(r);
    if (r.method === 'call.screening.bridge' && (bridgeFailure || state !== 'connected')) {
      output.write(JSON.stringify({ version: 1, epoch, requestId: r.requestId, ok: false, result: null, error: 'runtime_not_ready' }) + '\n'); return;
    }
    let result: unknown;
    switch (r.method) {
      case 'audio.probe': result = { state: 'completed', detected: false, peak: 0, samples: 480000, sampleRate: 48000, channels: 2, excludedProcessId: 123 }; break;
      case 'codec.test': result = { passed: false, codecs: ['G722', 'PCMA', 'PCMU', 'OPUS', 'EVS'].map(name => ({
        name, passed: name !== 'EVS', frames: name === 'EVS' ? 0 : 25, encodedBytes: 0, decodedSamples: 0, rms: 0,
        error: name === 'EVS' ? 'codec_library_missing' : null,
      })) }; break;
      case 'inventory': result = { devices: [{ id: 'capture', name: 'Fixture cable', direction: 'capture', state: 'Active' }],
        codecs: [{ name: 'G722', payload: 9, pcmRate: 16000, rtpClockRate: 8000, channels: 1 }], processLoopbackSupported: true }; break;
      case 'call.features': result = { access: 'trusted', disableCodecUpgrade: false, screening: 'none', command: null, commandSequence: 0,
        commands: [], commandOverflow: false }; break;
      case 'call.windows.connect': result = { callId: activeId, state: 'open', route: null }; break;
      case 'call.screening.prepare': screening = 'waiting'; result = { prepared: true }; break;
      case 'call.screening.bridge': screening = 'bridged'; result = { callId: activeId, state: 'open', route: null }; break;
      case 'status': result = { configured: true, endpoint: '127.0.0.1:5060', registration,
        micro: { state: 'ready', generation: 4294967296, errorCode: null }, call: activeId ? observation() : null,
        media: activeId ? { closed: state === 'local_ended', failed: false, sentPackets: 0, receivedPackets: 0,
          receive: { queuedPackets: 0, maximumPackets: 8, reorderMs: 20, duplicatePackets: 0, latePackets: 0, overflowPackets: 0,
            foreignPackets: 0, gatedPackets: 0, missingPackets: 0, maximumResidenceMs: 0 }, audio: { callId: activeId, state: audioState, route: null } } : null }; break;
      case 'call.prepare': activeId = String(r.params['callId']); direction = 'outgoing'; state = 'prepared'; result = observation(); break;
      case 'call.claim': assert.equal(r.params['callId'], activeId); assert.equal(direction, 'incoming'); result = observation(); break;
      case 'call.desktop.launch': result = { callId: activeId, action: 'launch', result: { phase: launchPhase,
        identity: launchPhase === 'ready' ? desktop : null, activationPid: launchPhase === 'submitted' ? 123 : null, errorCode: null } }; break;
      case 'desktop.observe': result = desktopWaitingReads-- > 0 ? { state: 'waiting', identity: null } : { state: 'ready', identity: desktop }; break;
      case 'call.audio.prepare': result = { callId: activeId, state: 'suspended', route: null }; break;
      case 'call.audio.rebind': audioState = 'open'; result = { callId: activeId, state: 'open', route: null }; break;
      case 'call.desktop.startVoice': result = { callId: r.params['callId'], action: 'startVoice', result: { state: startState, dispatch, reason: null } }; break;
      case 'call.desktop.pauseVoice': result = { callId: r.params['callId'], action: 'pauseVoice', generation: r.params['generation'], result: { state: 'stopped', dispatch, reason: null } }; break;
      case 'call.desktop.resumeVoice': result = { callId: r.params['callId'], action: 'resumeVoice', generation: r.params['generation'], result: { state: 'active', dispatch, reason: null } }; break;
      case 'call.desktop.stopVoice': result = { callId: r.params['callId'], action: 'stopVoice', result: { state: stopState, dispatch, reason: null } }; break;
      case 'call.dial': case 'call.answer': state = 'connected'; result = observation(); break;
      case 'call.waiting.end': result = { callId: activeId, ended: true }; break;
      case 'call.command.feedback': result = { callId: activeId, commandSequence: r.params['commandSequence'], success: r.params['success'] }; break;
      case 'call.hangup': state = 'local_ended'; result = observation(); break;
      case 'call.release': result = { callId: activeId, released: true }; activeId = null; break;
      case 'desktop.captureOwner': result = { endpointId: r.params['endpointId'], state: captureState,
        identity: captureState === 'matched' ? { instanceId: 'capture-instance', pid: 321, processStartTimeUtcTicks: '1' } : null }; break;
      default: throw new Error('Unexpected fixture request: ' + r.method);
    }
    output.write(JSON.stringify({ version: 1, epoch, requestId: r.requestId, ok: true, result, error: null }) + '\n');
  };
  input.on('data', bytes => { const r = JSON.parse(String(bytes).trim()) as Request; requests.push(r); void respond(r).catch(error => errors.push(String(error))); });
  const flows: PhoneFlow[] = [];
  const bound = new Map<string, string>(), prompts: { generation: number; threadId: string; prompt: string }[] = [];
  const promptSelections: unknown[] = [];
  const selectionUpdates: { threadId: string; command: PhoneVoiceSelectionCommand; selection: unknown }[] = [];
  let voicePrepares = 0;
  let failPrompt = false, failSelection = false, promptError = 'app_tools_unavailable';
  const defaultVoiceTasks: Pick<PhoneVoiceTasks, 'prepare' | 'bind' | 'prompt' | 'select' | 'release'> = {
    async prepare(_call, _database, _settings, generation = 0) { voicePrepares++; return generation ? randomUUID() : null; },
    async bind(call, _database, _settings, generation, preparedThreadId) {
      const key = call.callId + ':' + generation, threadId = preparedThreadId ?? bound.get(key) ?? randomUUID(); bound.set(key, threadId);
      if (!journal.callCommand(call.callId, 'call.bindVoice', generation)) {
        const intent = { epoch: call.epoch, callId: call.callId, operationId: randomUUID(), method: 'call.bindVoice' as const,
          ...(generation ? { voiceGeneration: generation } : {}), threadId, requestHash: 'sha256:' + '0'.repeat(64) };
        journal.submit(intent); journal.finishVoiceBinding(intent);
      }
      return threadId;
    },
    async prompt(call, _database, _settings, generation, threadId, prompt, beforeSubmit, selection, forwarded) {
      await beforeSubmit();
      if (failPrompt) throw new IvyError(promptError, 'The greeting is unavailable.');
      prompts.push({ generation, threadId, prompt });
      promptSelections.push(selection);
      if (forwarded || !journal.callCommand(call.callId, 'call.promptVoice', generation)) {
        const intent = { epoch: call.epoch, callId: call.callId, operationId: forwarded?.operationId ?? randomUUID(),
          method: forwarded ? 'call.forwardVoice' as const : 'call.promptVoice' as const,
          ...(generation ? { voiceGeneration: generation } : {}), threadId, prompt, requestHash: forwarded?.requestHash ?? 'sha256:' + '0'.repeat(64) };
        journal.submit(intent); journal.finishVoicePrompt(intent, 'sent');
      }
    },
    async select(call, _database, _settings, threadId, command, selection, beforeSubmit) {
      await beforeSubmit();
      if (failSelection) throw new IvyError('app_tools_unavailable', 'The model update failed.');
      assert.equal(journal.voiceTask(call.callId, journal.latestVoiceGeneration(call.callId)!), threadId);
      selectionUpdates.push({ threadId, command, selection });
      const values = { threadId, prompt: 'Model selection', ...selection,
        ...('commandSequence' in command ? { commandSequence: command.commandSequence } : {}) };
      const intent = { epoch: call.epoch, callId: call.callId,
        operationId: 'operationId' in command ? command.operationId : randomUUID(),
        method: 'call.selectVoice' as const, ...values, requestHash: digest(canonical(values)) };
      journal.submit(intent); journal.finishVoiceSelection(intent, 'sent');
    },
    async release() {},
  };
  const defaultArchives: Pick<PhoneVoiceArchive, 'run' | 'reconcile'> = { async reconcile() {}, async run(_call, _settings, idle) { await idle(); } };
  const flow = (value: PhoneFlowSettings = settings, archives: Pick<PhoneVoiceArchive, 'run' | 'reconcile'> = defaultArchives,
    voiceTasks: Pick<PhoneVoiceTasks, 'prepare' | 'bind' | 'prompt' | 'select' | 'release'> = defaultVoiceTasks,
    audioOwnerProof?: (call: import('../services/phone-bridge/src/runtime/admission.js').PhoneCall,
      target: import('../services/phone-bridge/src/runtime/journal.js').PhoneCallTarget, threadId: string) => Promise<void>) => {
    const f = new PhoneFlow(calls, native, value, async () => { credentialReads++; return { username: 'fixture', password: 'never-retained' }; },
      (_id, code) => errors.push(code), archives, undefined, voiceTasks, undefined, audioOwnerProof); flows.push(f); return f;
  };
  t.after(async () => {
    for (const f of flows) await f.close().catch(() => undefined);
    native.close(); input.destroy(); output.destroy(); journal.close();
    assert.ok(relative(tmpdir(), root).startsWith('ivy-phone-flow-')); rmSync(root, { recursive: true, force: true });
  });
  return { journal, native, calls, flow, requests, errors, epoch, root,
    screening: (value: string, fail = false) => { screening = value; bridgeFailure = fail; },
    registration: (value: Record<string, unknown> | null) => { registration = value; },
    methods: () => requests.map(r => r.method), credentialReads: () => credentialReads,
    intercept: (value: (r: Request) => Promise<void>) => { intercept = value; },
    voice: (start: string, stop: string) => { startState = start; stopState = stop; },
    capture: (value: 'none' | 'matched' | 'ambiguous' | 'unavailable') => { captureState = value; },
    audio: (value: string) => { audioState = value; },
    launch: (phase: string) => { launchPhase = phase; },
    desktopWaiting: (reads: number) => { desktopWaitingReads = reads; },
    voicePrepares: () => voicePrepares,
    failPrompt: (value: boolean, code = 'app_tools_unavailable') => { failPrompt = value; promptError = code; },
    failSelection: (value: boolean) => { failSelection = value; },
    ended: () => { state = 'local_ended'; }, prompts, promptSelections, selectionUpdates,
    activate: (callId: string) => { activeId = callId; direction = 'outgoing'; state = 'prepared'; },
    command: (value: string | null, sequence: number, model: string | null = null, effort: string | null = null) => {
      command = value; commandSequence = sequence; commandModel = model; commandEffort = effort;
      if (value === 'new_voice' || value === 'select_voice') commands.push({ command: value, sequence, model, reasoningEffort: effort });
    },
    incoming: () => { activeId = randomUUID(); direction = 'incoming'; state = 'ringing'; incoming = {
      sipCallId: 'wire', fromUri: 'sip:known@fixture', peerAddress: '127.0.0.1', peerPort: 5060, transport: 'udp' }; return activeId; } };
}

test('Windows call connects and cleans up without launching Desktop or Voice, and route is part of idempotency', async t => {
  const f = fixture(t), flow = f.flow({ ...settings, audio: { ...settings.audio, sourceMode: 'system_excluding_runtime' } });
  const id = randomUUID(), call = await flow.request('main', id, 'personal', 'windows');
  await until(() => !!f.journal.callCommand(call.callId, 'call.windows.connect')?.receipt);
  assert.ok(!f.methods().some(method => method.startsWith('call.desktop.')));
  assert.equal((await flow.request('main', id, 'personal', 'windows')).callId, call.callId);
  await assert.rejects(flow.request('main', id, 'personal', 'voice'));
  await flow.hangup('main', call.callId);
  assert.equal(f.journal.currentCall(), null);
  assert.equal(f.methods().filter(method => method === 'call.dial').length, 1);
});

test('Challenge admission is scoped to the provider peer while an asserted trusted identity retains its bypass', () => {
  const policy = new PhoneAdmission({ recipients: [], incoming: [
    { peerAddress: '127.0.0.1', transport: 'udp', fromUri: 'sip:known@fixture', expectedAssertedNumber: '+49123456789' },
  ], challengedIncoming: [{ peerAddress: '127.0.0.1', transport: 'udp' }] });
  const context = { sipCallId: 'wire', fromUri: 'sip:known@fixture', peerAddress: '127.0.0.1', peerPort: 5060, transport: 'udp' as const, assertedNumbers: ['+49123456789'] };
  assert.equal(policy.challengesIncoming(context), false);
  assert.equal(policy.challengesIncoming({ ...context, assertedNumbers: [] }), true);
  assert.equal(policy.challengesIncoming({ ...context, peerAddress: '127.0.0.2', assertedNumbers: [] }), false);
});

for (const outcome of ['connected', 'device-failure', 'cancelled'] as const) test('screening bridge lifecycle: ' + outcome, async t => {
  const f = fixture(t), credentialsPath = join(f.root, 'speech.json');
  writeFileSync(credentialsPath, JSON.stringify({schemaVersion:1,speech:{apiKey:'fixture-only'}}));
  const wav = Buffer.alloc(48); wav.write('RIFF'); wav.write('WAVE', 8);
  let synthesis = 0;
  t.mock.method(globalThis, 'fetch', async () => { synthesis++; return new Response(wav); });
  const flow = f.flow({ ...settings, speech: { credentialsPath, endpoint: 'https://fixture.invalid/speech', model: 'fixture', voice: 'fixture' } });
  const bridge = new PhoneBridge(flow, f.native), operationId = randomUUID();
  const call = await flow.screen('main', operationId, 'personal', { announcement: 'Fixture announcement', timeoutSeconds: 30, repeatCount: 1 });
  await until(() => !!f.journal.callCommand(call.callId, 'call.dial')?.receipt);
  await assert.rejects(bridge.invoke('bridgeScreening', { callId: call.callId }, context()), { code: 'phone_screening_not_accepted' });
  assert.equal(f.journal.callCommand(call.callId, 'call.screening.bridge'), null);
  assert.ok(f.methods().indexOf('call.screening.prepare') < f.methods().indexOf('call.dial'));
  f.screening('accepted', outcome === 'device-failure');
  if (outcome === 'cancelled') {
    const gate = deferred();
    f.intercept(async request => { if (request.method === 'call.screening.bridge') await gate.promise; });
    const pending = bridge.invoke('bridgeScreening', { callId: call.callId }, context());
    const refused = assert.rejects(pending);
    await until(() => f.methods().includes('call.screening.bridge'));
    try { await flow.hangup('main', call.callId); }
    finally { gate.resolve(); }
    await refused;
  } else if (outcome === 'device-failure') {
    await assert.rejects(bridge.invoke('bridgeScreening', { callId: call.callId }, context()));
  } else {
    await bridge.invoke('bridgeScreening', { callId: call.callId }, context());
    await bridge.invoke('bridgeScreening', { callId: call.callId }, context());
    assert.equal(f.methods().filter(method => method === 'call.screening.bridge').length, 1);
    await flow.hangup('main', call.callId);
  }
  assert.equal(synthesis, 1);
  assert.equal(f.journal.currentCall(call.callId), null);
  assert.equal(f.methods().filter(method => method === 'call.dial').length, 1);
  assert.ok(!f.methods().some(method => method.startsWith('call.desktop.')));
  assert.equal(f.journal.callCommand(call.callId, 'call.screening.bridge')?.receipt?.ok, outcome === 'connected');
});

test('Phone releases admission even when optional Voice archival fails', async t => {
  const f = fixture(t), gate = deferred(); let entered = false;
  const archiveSettings = { databasePath: join(f.root, 'desktop.sqlite'), appTools: { nodeExecutable: process.execPath,
    nodeExecutableHash: 'sha256:' + '0'.repeat(64), serverPath: join(f.root, 'server.mjs'), serverHash: 'sha256:' + '0'.repeat(64),
    pipePath: '\\\\.\\pipe\\fixture', actorThreadId: randomUUID() } };
  const flow = f.flow({ ...settings, voiceArchive: archiveSettings }, { async reconcile() {}, async run(call, original, idle) {
    assert.deepEqual(original, archiveSettings);
    assert.equal(f.journal.callCommand(call.callId, 'call.desktop.stopVoice')?.phase, 'result');
    assert.equal(f.methods().includes('call.release'), false);
    await idle(); entered = true; await gate.promise; throw new Error('archive unavailable');
  } });
  const call = await flow.request('main', randomUUID(), 'personal');
  await until(() => !!f.journal.callCommand(call.callId, 'call.dial')?.receipt); await tick();
  const cleanup = flow.hangup('main', call.callId);
  try {
    await until(() => entered);
    await assert.rejects(flow.request('main', randomUUID(), 'personal'), { code: 'phone_call_busy' });
    assert.equal(f.methods().filter(method => method === 'call.desktop.startVoice').length, 1);
  } finally { gate.resolve(); await cleanup; }
  assert.equal(f.journal.currentCall(), null);
  assert.ok(f.methods().indexOf('call.desktop.stopVoice') < f.methods().indexOf('desktop.captureOwner'));
  assert.ok(f.methods().indexOf('desktop.captureOwner') < f.methods().indexOf('call.release'));
});

test('Phone flow retains its target before audio, connects once and cleans up the original after configuration change', async t => {
  const f = fixture(t), flow = f.flow(), operation = randomUUID();
  f.intercept(async r => {
    if (r.method === 'call.audio.prepare' || r.method === 'call.desktop.startVoice')
      assert.deepEqual(f.journal.target(String(r.params['callId']))?.desktop, desktop);
    if (r.method === 'call.desktop.startVoice') assert.equal(f.voicePrepares(), 1, 'the prepared controller is retained while ringing');
  });
  const call = await flow.request('main', operation, 'personal');
  await until(() => f.journal.callCommand(call.callId, 'call.waiting.end')?.phase === 'result'); await tick();
  assert.deepEqual(f.methods().filter(method => method !== 'status'),
    ['call.prepare', 'call.features', 'call.desktop.launch', 'call.audio.prepare', 'call.dial', 'call.desktop.startVoice', 'call.waiting.end']);
  assert.equal(f.requests.find(r => r.method === 'call.dial')?.params['waiting'], true);
  assert.deepEqual(await flow.request('main', operation, 'personal'), call); await tick();
  assert.equal(f.credentialReads(), 1);
  const changed = f.flow({ ...settings, voiceInput: { ...settings.voiceInput, hotkeyFallback: { ...settings.voiceInput.hotkeyFallback, focusBeforeHotkey: false } } });
  await changed.hangup('admin', call.callId);
  assert.deepEqual(f.requests.find(r => r.method === 'call.desktop.stopVoice')?.params['voiceInput'], settings.voiceInput);
  assert.equal(f.journal.currentCall(), null); assert.deepEqual(f.errors, []);
  assert.ok(!JSON.stringify(f.journal.target(call.callId)).includes('never-retained'));
});

test('configured Voice defaults reach the complete first prompt and explicit selection can override them', async t => {
  const f = fixture(t), voiceDefault = { model: 'gpt-6-sol', reasoningEffort: 'low' } as const;
  const flow = f.flow({ ...settings, voiceDefault });
  assert.deepEqual(flow.configuration().voiceDefault, voiceDefault);
  const call = await flow.request('main', randomUUID(), 'personal', 'voice', undefined,
    'Ask whether Tuesday at 10 or Thursday at 15 works, then confirm the caller\'s choice.');
  await until(() => f.journal.callCommand(call.callId, 'call.promptVoice')?.phase === 'result');
  await tick();
  assert.deepEqual(flow.voiceSelection(call.callId), voiceDefault);
  assert.deepEqual(f.promptSelections, [voiceDefault]);
  assert.ok(f.prompts[0]!.prompt.includes('Tuesday at 10 or Thursday at 15'));
  await flow.controlVoice('main', call.callId, randomUUID(),
    { action: 'select', selection: { model: 'gpt-6-sol', reasoningEffort: 'high' } });
  assert.deepEqual(flow.voiceSelection(call.callId), { model: 'gpt-6-sol', reasoningEffort: 'high' });
  await flow.hangup('main', call.callId);
});

test('a timed-out call creation waits for its original native owner before spending hangup', async t => {
  const f = fixture(t), flow = f.flow();
  let creation: PhoneIntent | null = null;
  t.mock.method(f.calls, 'prepare', async (_principalId: string, callId: string) => {
    creation = { epoch: f.epoch, operationId: randomUUID(), callId, method: 'call.prepare', requestHash: 'sha256:' + '0'.repeat(64) };
    f.journal.submit(creation);
    throw new IvyError('phone_request_timeout', 'The native creation has not returned.', 'unknown');
  });
  const call = await flow.request('main', randomUUID(), 'personal');
  await until(() => f.errors.includes('phone_request_timeout')); await tick();
  assert.equal(f.journal.currentCall(call.callId)?.callId, call.callId);
  assert.ok(!f.methods().includes('call.hangup'));
  assert.ok(creation);
  f.activate(call.callId);
  f.journal.finish(creation, { version: 1, epoch: f.epoch, requestId: 1, ok: true, result: { id: call.callId, state: 'prepared' }, error: null });
  await flow.observe(); await tick();
  if (f.journal.currentCall(call.callId)) await flow.observe();
  assert.equal(f.journal.currentCall(call.callId), null);
  assert.equal(f.methods().filter(method => method === 'call.hangup').length, 1);
});

for (const direction of ['incoming', 'outgoing'] as const) test('Voice call requests forward to the active task once: ' + direction, async t => {
  const f = fixture(t, undefined, undefined, {
    recipients: [{ id: 'personal', destination: 'sip:known@fixture' }],
    incoming: [{ peerAddress: '127.0.0.1', transport: 'udp', fromUri: 'sip:known@fixture' }],
  }), flow = f.flow();
  const call = direction === 'incoming' ? await flow.accept('main', randomUUID(), f.incoming())
    : await flow.request('main', randomUUID(), 'personal', 'voice', undefined, 'Opening request');
  await until(() => f.journal.callCommand(call.callId, 'call.promptVoice')?.phase === 'result');
  await tick();
  await flow.controlVoice('main', call.callId, randomUUID(), { action: 'select', selection: { model: 'gpt-6-luna', reasoningEffort: 'low' } });
  await flow.controlVoice('main', call.callId, randomUUID(), { action: 'restart' });
  const threadId = f.journal.voiceTask(call.callId, 1), baseline = f.prompts.length;
  const operationId = randomUUID();
  const forwarded = await Promise.all([
    flow.request('another-agent', operationId, 'personal', 'voice', undefined, 'Additional request'),
    flow.request('another-agent', operationId, 'personal', 'voice', undefined, 'Additional request'),
    flow.request('main', randomUUID(), 'personal', 'voice', undefined, 'Another request'),
  ]);
  assert.ok(forwarded.every(value => value.callId === call.callId));
  assert.deepEqual(f.prompts.slice(baseline), [
    { generation: 1, threadId, prompt: 'Additional request' },
    { generation: 1, threadId, prompt: 'Another request' },
  ]);
  assert.deepEqual(f.promptSelections.slice(baseline), Array(2).fill({ model: 'gpt-6-luna', reasoningEffort: 'low' }));
  assert.equal(f.methods().filter(method => method === 'call.dial').length, direction === 'outgoing' ? 1 : 0);
  assert.equal(f.journal.get(operationId)?.intent.method, 'call.forwardVoice');
  assert.throws(() => flow.request('another-agent', operationId, 'personal', 'voice', undefined, 'Changed request'), { code: 'mutation_conflict' });
  assert.throws(() => flow.request('different-agent', operationId, 'personal', 'voice', undefined, 'Additional request'), { code: 'mutation_conflict' });
  await flow.hangup('main', call.callId);
  assert.equal((await flow.request('another-agent', operationId, 'personal', 'voice', undefined, 'Additional request')).callId, call.callId);
  assert.equal(f.prompts.length, baseline + 2);
  assert.equal(f.journal.currentCall(), null);
});

test('Voice prompt forwarding waits for original setup and refuses a call that ends before submission', async t => {
  const f = fixture(t), flow = f.flow(), entered = deferred(), resume = deferred();
  f.intercept(async request => { if (request.method === 'call.dial') { entered.resolve(); await resume.promise; } });
  const call = await flow.request('main', randomUUID(), 'personal', 'voice', undefined, 'Opening request');
  await entered.promise;
  const forwarded = flow.request('main', randomUUID(), 'personal', 'voice', undefined, 'After setup');
  assert.equal(f.prompts.length, 0);
  resume.resolve();
  assert.equal((await forwarded).callId, call.callId);
  assert.equal(f.prompts.at(-1)?.prompt, 'After setup');
  f.ended();
  await assert.rejects(flow.request('main', randomUUID(), 'personal', 'voice', undefined, 'Too late'), { code: 'phone_call_cancelled' });
  assert.equal(f.prompts.length, 2);
  assert.equal(f.methods().filter(method => method === 'call.dial').length, 1);
});

test('Voice forwarding requires the same recipient and leaves Windows call admission unchanged', async t => {
  const f = fixture(t, undefined, undefined, {
    recipients: [{ id: 'personal', destination: 'sip:personal@fixture' }, { id: 'other', destination: 'sip:other@fixture' }], incoming: [],
  }), flow = f.flow();
  const call = await flow.request('main', randomUUID(), 'personal', 'voice', undefined, 'Opening request');
  await until(() => f.journal.callCommand(call.callId, 'call.promptVoice')?.phase === 'result'); await tick();
  await assert.rejects(flow.request('main', randomUUID(), 'other', 'voice', undefined, 'For someone else'), { code: 'phone_call_busy' });
  await assert.rejects(flow.request('main', randomUUID(), 'personal', 'windows'), { code: 'phone_call_busy' });
  assert.equal(f.prompts.length, 1);
  await flow.hangup('main', call.callId);
  const windows = await flow.request('main', randomUUID(), 'personal', 'windows');
  await until(() => f.journal.callCommand(windows.callId, 'call.windows.connect')?.phase === 'result'); await tick();
  await assert.rejects(flow.request('main', randomUUID(), 'personal', 'voice', undefined, 'Needs Voice'), { code: 'phone_call_busy' });
  assert.equal(f.prompts.length, 1);
});

test('Voice keypad selection keeps the current task and *0# transfers Voice to a new generation', async t => {
  const f = fixture(t), archived: { generation?: number; threadId?: string }[] = [];
  const archives = { async reconcile() {}, async run(_call: unknown, _settings: unknown, idle: () => Promise<void>, scope?: { generation?: number; threadId?: string }) {
    await idle(); archived.push({ ...scope });
  } };
  const flow = f.flow(settings, archives);
  const call = await flow.request('main', randomUUID(), 'personal', 'voice', undefined, 'Introduce the scheduled alert.');
  await until(() => f.journal.callCommand(call.callId, 'call.promptVoice', 0)?.phase === 'result');
  const first = f.journal.voiceTask(call.callId, 0);
  assert.ok(first);
  const opening = 'Begrüße die angerufene Person zuerst kurz auf Deutsch.\n\nIntroduce the scheduled alert.';
  assert.deepEqual(f.prompts, [{ generation: 0, threadId: first, prompt: opening }]);
  assert.deepEqual(f.promptSelections, [{ model: 'gpt-6-sol', reasoningEffort: 'high' }]);
  f.command('select_voice', 1, 'luna', 'low'); await flow.observe();
  await until(() => f.selectionUpdates.length === 1);
  await until(() => f.requests.some(r => r.method === 'call.command.feedback' && r.params['commandSequence'] === 1));
  await until(() => f.journal.feedbackCommand(call.callId, 1)?.phase === 'result'); await tick();
  assert.equal(f.requests.find(r => r.method === 'call.command.feedback' && r.params['commandSequence'] === 1)?.params['success'], true);
  assert.deepEqual(flow.voiceSelection(call.callId), { model: 'gpt-6-luna', reasoningEffort: 'low' });
  assert.deepEqual(f.selectionUpdates, [{ threadId: first, command: { commandSequence: 1 },
    selection: { model: 'gpt-6-luna', reasoningEffort: 'low' } }]);
  assert.equal(f.journal.latestVoiceGeneration(call.callId), 0);
  assert.equal(f.methods().filter(method => method === 'call.desktop.pauseVoice').length, 0);
  f.command('new_voice', 2); await flow.observe();
  await until(() => f.journal.callCommand(call.callId, 'call.promptVoice', 1)?.phase === 'result');
  await until(() => f.requests.some(r => r.method === 'call.command.feedback' && r.params['commandSequence'] === 2));
  const second = f.journal.voiceTask(call.callId, 1);
  assert.ok(second && second !== first);
  assert.deepEqual(archived, []);
  assert.deepEqual(f.prompts, [
    { generation: 0, threadId: first, prompt: opening },
    { generation: 1, threadId: second, prompt: opening },
  ]);
  assert.deepEqual(f.promptSelections, [
    { model: 'gpt-6-sol', reasoningEffort: 'high' },
    { model: 'gpt-6-luna', reasoningEffort: 'low' },
  ]);
  assert.equal(f.methods().filter(method => method === 'call.dial').length, 1);
  assert.equal(f.methods().filter(method => method === 'call.desktop.pauseVoice').length, 0);
  assert.equal(f.methods().filter(method => method === 'call.desktop.resumeVoice').length, 0);
  await flow.hangup('main', call.callId);
  assert.deepEqual(archived, [{ generation: 0, threadId: first }, { generation: 1, threadId: second }]);
});

for (const sameOwner of [true, false]) test('failed Voice audio uses one original rebind only when the task owner is confirmed: ' + sameOwner, async t => {
  const f = fixture(t);
  let checks = 0;
  const flow = f.flow(undefined, undefined, undefined, async (call, target, threadId) => {
    checks++;
    assert.equal(f.journal.voiceTask(call.callId, 0), threadId);
    assert.equal(target.desktop['pid'], desktop.pid);
    if (!sameOwner) throw new IvyError('phone_audio_owner_changed', 'Different task now owns Voice.');
  });
  const call = await flow.request('main', randomUUID(), 'personal', 'voice');
  await until(() => f.journal.callCommand(call.callId, 'call.promptVoice')?.phase === 'result');
  f.audio('failed'); await flow.observe();
  if (sameOwner) {
    await until(() => f.journal.callCommand(call.callId, 'call.audio.rebind')?.phase === 'result');
    assert.equal(f.journal.currentCall(call.callId)?.callId, call.callId);
    assert.equal(f.methods().filter(method => method === 'call.audio.rebind').length, 1);
    f.audio('failed'); await flow.observe();
  }
  await until(() => f.journal.currentCall(call.callId) === null);
  assert.equal(checks, 1);
  assert.equal(f.methods().filter(method => method === 'call.audio.rebind').length, sameOwner ? 1 : 0);
  if (!sameOwner) assert.ok(f.errors.includes('phone_audio_owner_changed'));
});

test('Voice selection and restart both run in order when entered between polls', async t => {
  const f = fixture(t), flow = f.flow();
  const call = await flow.request('main', randomUUID(), 'personal', 'voice', undefined, 'Start the call.');
  await until(() => f.journal.callCommand(call.callId, 'call.promptVoice', 0)?.phase === 'result');
  f.command('select_voice', 1, 'astra', 'xhigh');
  f.command('new_voice', 2, 'astra', 'xhigh');
  await flow.observe();
  await until(() => f.selectionUpdates.length === 1);
  await until(() => f.journal.feedbackCommand(call.callId, 1)?.phase === 'result'); await tick();
  await flow.observe();
  await until(() => f.journal.callCommand(call.callId, 'call.promptVoice', 1)?.phase === 'result');
  assert.deepEqual(f.selectionUpdates[0]?.command, { commandSequence: 1 });
  assert.deepEqual(flow.voiceSelection(call.callId), { model: 'gpt-6-astra', reasoningEffort: 'xhigh' });
  assert.deepEqual(f.promptSelections, [
    { model: 'gpt-6-sol', reasoningEffort: 'high' },
    { model: 'gpt-6-astra', reasoningEffort: 'xhigh' },
  ]);
  await flow.hangup('main', call.callId);
});

for (const direction of ['incoming', 'outgoing'] as const) test('MCP Voice controls preserve model and results across retries and archival: ' + direction, async t => {
  const f = fixture(t), flow = f.flow(), bridge = new PhoneBridge(flow, f.native);
  const call = direction === 'incoming' ? await flow.accept('main', randomUUID(), f.incoming())
    : await flow.request('main', randomUUID(), 'personal', 'voice');
  await until(() => f.journal.callCommand(call.callId, 'call.promptVoice')?.phase === 'result'); await tick();
  const selected = { callId: call.callId, operationId: randomUUID(), model: 'gpt-6-astra', reasoningEffort: 'ultra' };
  const selection = await bridge.invoke('selectVoice', selected, context());
  assert.deepEqual(await bridge.invoke('selectVoice', selected, context()), selection);
  assert.equal(f.selectionUpdates.length, 1);
  assert.equal(f.journal.latestVoiceGeneration(call.callId), 0);
  const restarted = { callId: call.callId, operationId: randomUUID() };
  const preparesBeforeRestart = f.voicePrepares();
  const restart = await bridge.invoke('restartVoice', restarted, context());
  assert.equal(f.journal.latestVoiceGeneration(call.callId), 1);
  assert.equal(f.journal.get(restarted.operationId)?.receipt?.ok, true);
  assert.deepEqual(f.promptSelections[1], { model: 'gpt-6-astra', reasoningEffort: 'ultra' });
  assert.deepEqual(await bridge.invoke('restartVoice', restarted, context()), restart);
  assert.equal(f.voicePrepares(), preparesBeforeRestart + 1, 'repeated MCP restart performs no additional preparation');
  assert.equal(f.methods().filter(method => method === (direction === 'incoming' ? 'call.answer' : 'call.dial')).length, 1);
  assert.equal(f.methods().includes('call.hangup'), false);
  assert.equal(f.methods().includes('call.command.feedback'), false, 'MCP commands do not consume keypad sequences');
  await assert.rejects(bridge.invoke('selectVoice', { ...selected, reasoningEffort: 'low' }, context()), { code: 'mutation_conflict' });
  await assert.rejects(bridge.invoke('restartVoice', selected, context()), { code: 'invalid_arguments' });
  await assert.rejects(bridge.invoke('restartVoice', { callId: call.callId, operationId: selected.operationId }, context()), { code: 'mutation_conflict' });
  f.command('select_voice', 1, 'luna', 'low'); await flow.observe();
  await until(() => f.journal.feedbackCommand(call.callId, 1)?.phase === 'result'); await tick();
  assert.deepEqual(await bridge.invoke('selectVoice', selected, context()), selection);
  assert.deepEqual(flow.voiceSelection(call.callId), { model: 'gpt-6-luna', reasoningEffort: 'low' }, 'replay does not revert a newer choice');
  await flow.hangup('main', call.callId);
  f.journal.loseEpoch(f.epoch); f.journal.beginEpoch(randomUUID());
  assert.ok(f.journal.archiveStatus().records > 0);
  assert.deepEqual(await bridge.invoke('restartVoice', restarted, context()), restart);
  assert.deepEqual(await bridge.invoke('operation', restarted, context()), restart);
  assert.deepEqual(await bridge.invoke('operation', { callId: call.callId, operationId: selected.operationId }, context()), selection);
  await assert.rejects(bridge.invoke('restartVoice', { ...restarted, operationId: randomUUID() }, context()), { code: 'phone_call_cancelled' });
});

test('MCP Voice controls reject invalid selections and Windows calls before effects', async t => {
  const f = fixture(t), flow = f.flow(), bridge = new PhoneBridge(flow, f.native);
  const call = await flow.request('main', randomUUID(), 'personal', 'windows');
  await until(() => f.journal.callCommand(call.callId, 'call.windows.connect')?.phase === 'result'); await tick();
  const args = { callId: call.callId, operationId: randomUUID() }, before = f.requests.length;
  for (const values of [
    { model: 'gpt-6-luna', reasoningEffort: 'ultra' }, { model: 'unknown', reasoningEffort: 'high' },
    { model: 'gpt-6-sol', reasoningEffort: 'minimal' },
  ]) await assert.rejects(bridge.invoke('selectVoice', { ...args, ...values }, context()), { code: 'invalid_arguments' });
  await assert.rejects(bridge.invoke('restartVoice', args, context()), { code: 'phone_voice_task_missing' });
  await assert.rejects(bridge.invoke('selectVoice', { ...args, model: 'gpt-6-sol', reasoningEffort: 'high' }, context()), { code: 'phone_voice_task_missing' });
  assert.equal(f.requests.length, before);
  assert.equal(f.journal.get(args.operationId), null);
  await assert.rejects(bridge.invoke('operation', { ...args, operationId: f.journal.callCommand(call.callId, 'call.dial')!.intent.operationId, method: 'call.dial' }, context()), { code: 'invalid_arguments' });
});

test('MCP restart retains a lost greeting outcome and never creates another task on replay', async t => {
  const f = fixture(t), flow = f.flow(), bridge = new PhoneBridge(flow, f.native);
  const call = await flow.request('main', randomUUID(), 'personal', 'voice');
  await until(() => f.journal.callCommand(call.callId, 'call.promptVoice')?.phase === 'result'); await tick();
  f.failPrompt(true);
  const preparesBeforeRestart = f.voicePrepares();
  const args = { callId: call.callId, operationId: randomUUID() };
  const original = await bridge.invoke('restartVoice', args, context());
  assert.equal(f.journal.get(args.operationId)?.receipt?.error, 'operation_outcome_unknown');
  assert.equal(f.journal.latestVoiceGeneration(call.callId), 1);
  f.failPrompt(false);
  assert.deepEqual(await bridge.invoke('restartVoice', args, context()), original);
  assert.equal(f.voicePrepares(), preparesBeforeRestart + 1);
  assert.ok(f.journal.currentCall(call.callId), 'the existing call remains usable');
});

test('MCP Voice controls share keypad sequencing and hangup cancels pending changes', async t => {
  const f = fixture(t), flow = f.flow(), bridge = new PhoneBridge(flow, f.native), gate = deferred();
  const call = await flow.request('main', randomUUID(), 'personal', 'voice');
  await until(() => f.journal.callCommand(call.callId, 'call.promptVoice')?.phase === 'result'); await tick();
  const preparesBeforeRestart = f.voicePrepares();
  let reads = 0;
  f.intercept(async r => { if (r.method === 'status' && reads++ === 0) await gate.promise; });
  const args = { callId: call.callId, operationId: randomUUID() };
  const pending = bridge.invoke('restartVoice', args, context());
  const cancelled = assert.rejects(pending, { code: 'phone_call_cancelled' });
  await until(() => reads > 0);
  await assert.rejects(bridge.invoke('selectVoice', { ...args, operationId: randomUUID(), model: 'gpt-6-sol', reasoningEffort: 'high' }, context()), { code: 'phone_call_busy' });
  f.command('select_voice', 1, 'astra', 'max'); await flow.observe();
  assert.equal(f.selectionUpdates.length, 0, 'keypad waits for the same call slot');
  await flow.hangup('main', call.callId); gate.resolve(); await cancelled;
  assert.equal(f.voicePrepares(), preparesBeforeRestart, 'hangup prevents the new task preparation');
  assert.equal(f.journal.get(args.operationId), null);
});

test('outgoing Voice and its prompt wait for the peer to answer', async t => {
  const f = fixture(t), connected = deferred(), flow = f.flow();
  f.intercept(async request => {
    if (request.method === 'call.dial') await connected.promise;
  });
  const call = await flow.request('main', randomUUID(), 'personal', 'voice', undefined, 'How was your weekend?');
  await until(() => f.methods().includes('call.dial'));
  await until(() => f.voicePrepares() === 1);
  assert.ok(!f.methods().includes('call.desktop.startVoice'));
  assert.equal(f.journal.callCommand(call.callId, 'call.bindVoice'), null);
  assert.equal(f.journal.callCommand(call.callId, 'call.promptVoice'), null);
  connected.resolve();
  await until(() => f.journal.callCommand(call.callId, 'call.promptVoice')?.phase === 'result');
  assert.ok(f.methods().indexOf('call.dial') < f.methods().indexOf('call.desktop.startVoice'));
  assert.equal(f.voicePrepares(), 1);
  assert.deepEqual(f.prompts.map(value => value.prompt),
    ['Begrüße die angerufene Person zuerst kurz auf Deutsch.\n\nHow was your weekend?']);
  await flow.hangup('main', call.callId);
});

test('Voice prompt is not submitted after the peer ends the call during task binding', async t => {
  const f = fixture(t), gate = deferred(), threadId = randomUUID();
  let binding = false, submitted = false;
  const flow = f.flow(settings, undefined, {
    async prepare() { return threadId; },
    async bind() { binding = true; await gate.promise; return threadId; },
    async prompt(_call, _database, _settings, _generation, _threadId, _prompt, beforeSubmit) {
      await beforeSubmit();
      submitted = true;
    },
    async select() {},
    async release() {},
  });
  const call = await flow.request('main', randomUUID(), 'personal', 'voice', undefined, 'How was your weekend?');
  await until(() => binding);
  f.ended();
  gate.resolve();
  await until(() => f.journal.currentCall(call.callId) === null);
  assert.equal(submitted, false);
  assert.equal(f.journal.callCommand(call.callId, 'call.promptVoice'), null);
  assert.ok(f.errors.includes('phone_call_cancelled'));
});

test('failed outgoing task preparation is handled while the peer is still ringing', async t => {
  const f = fixture(t), ringing = deferred();
  let attempted = false;
  const flow = f.flow(settings, undefined, {
    async prepare() { attempted = true; throw new IvyError('app_tools_unavailable', 'Fixture App Tools failed.'); },
    async bind() { assert.fail('Failed preparation cannot bind Voice.'); },
    async prompt() { assert.fail('Failed preparation cannot prompt Voice.'); },
    async select() {},
    async release() {},
  });
  f.intercept(async request => { if (request.method === 'call.dial') await ringing.promise; });
  const call = await flow.request('main', randomUUID(), 'personal', 'voice');
  try {
    await until(() => attempted);
    await tick(); // An unhandled rejection here would fail the test/process.
    assert.ok(!f.methods().includes('call.desktop.startVoice'));
  } finally { ringing.resolve(); }
  await until(() => f.journal.currentCall(call.callId) === null);
  assert.ok(f.errors.includes('app_tools_unavailable'));
  assert.equal(f.methods().filter(method => method === 'call.dial').length, 1);
});

test('explicit Voice calls on a Windows-only service are refused before dialing', async t => {
  const f = fixture(t);
  for (const missing of ['task-control', 'input']) {
    const flow = f.flow({ ...settings, incomingRoute: 'windows', outgoingRoute: 'windows',
      ...(missing === 'task-control' ? { voiceArchive: null } : { voiceInput: { micro: null, hotkeyFallback: null } }),
    });
    assert.throws(() => flow.request('main', randomUUID(), 'personal', 'voice'), { code: 'phone_voice_unconfigured' });
    assert.deepEqual(f.methods(), []);
    assert.equal(f.journal.currentCall(), null);
  }
});

test('hangup fences Voice startup before asynchronous task release completes', async t => {
  const f = fixture(t), prepared = deferred(), releasing = deferred();
  let preparing = false, releaseStarted = false;
  const flow = f.flow(settings, undefined, {
    async prepare() { preparing = true; await prepared.promise; return randomUUID(); },
    async bind() { assert.fail('Cancelled preparation cannot bind Voice.'); },
    async prompt() {}, async select() {},
    async release() { releaseStarted = true; await releasing.promise; },
  });
  const call = await flow.request('main', randomUUID(), 'personal', 'voice');
  await until(() => preparing);
  const hangup = flow.hangup('main', call.callId);
  try {
    await until(() => releaseStarted);
    prepared.resolve();
    await tick(); await tick();
    assert.ok(!f.methods().includes('call.desktop.startVoice'));
  } finally { prepared.resolve(); releasing.resolve(); await hangup; }
  await until(() => f.journal.currentCall(call.callId) === null);
});

test('a prompted Voice call closes if the Codex task cannot be bound', async t => {
  const f = fixture(t), threadId = randomUUID();
  let prompted = false;
  const flow = f.flow(settings, undefined, {
    async prepare() { return threadId; },
    async bind() { throw new IvyError('phone_voice_task_missing', 'Fixture binding failed.'); },
    async prompt() { prompted = true; },
    async select() {},
    async release() {},
  });
  const call = await flow.request('main', randomUUID(), 'personal', 'voice', undefined, 'Tell the user why we called.');
  await until(() => f.journal.currentCall(call.callId) === null);
  assert.equal(prompted, false);
  assert.ok(f.errors.includes('phone_voice_task_missing'));
  assert.equal(f.methods().filter(method => method === 'call.dial').length, 1);
  assert.equal(f.methods().filter(method => method === 'call.hangup').length, 1);
});

test('Phone flow accepts only original incoming admission and never prepares or dials it', async t => {
  const f = fixture(t), id = f.incoming(), flow = f.flow();
  f.intercept(async r => {
    if (r.method === 'call.answer')
      assert.equal(f.journal.callCommand(id, 'call.bindVoice'), null);
  });
  const call = await flow.accept('main', randomUUID(), id);
  await until(() => !!f.journal.callCommand(id, 'call.answer')?.receipt); await tick();
  await until(() => f.journal.callCommand(id, 'call.promptVoice')?.phase === 'result');
  assert.equal(call.callId, id); assert.equal(f.credentialReads(), 0);
  assert.deepEqual(f.prompts.map(value => value.prompt),
    ['Greet the user with "Hi"']);
  assert.ok(f.methods().indexOf('call.claim') < f.methods().indexOf('call.desktop.launch'));
  assert.ok(f.methods().indexOf('call.answer') < f.methods().indexOf('call.desktop.launch'));
  assert.equal(f.journal.callCommand(id, 'call.bindVoice')?.phase, 'result');
  assert.equal(f.journal.callCommand(id, 'call.waiting.end')?.phase, 'result');
  assert.equal(f.requests.find(r => r.method === 'call.answer')?.params['waiting'], true);
  assert.ok(!f.methods().includes('call.prepare') && !f.methods().includes('call.dial'));
  f.ended(); await flow.observe(); assert.equal(f.journal.currentCall(), null); assert.deepEqual(f.errors, []);
});

test('configured incoming greeting is submitted only after the call connects', async t => {
  const f = fixture(t), id = f.incoming();
  const flow = f.flow({ ...settings, incomingInitialPrompt: 'Begruesse den User mit "Servus"' });
  f.intercept(async r => {
    if (r.method === 'call.answer')
      assert.equal(f.journal.callCommand(id, 'call.promptVoice'), null);
  });
  await flow.accept('main', randomUUID(), id);
  await until(() => f.journal.callCommand(id, 'call.promptVoice')?.phase === 'result');
  assert.deepEqual(f.prompts.map(value => value.prompt), ['Begruesse den User mit "Servus"']);
  f.command('new_voice', 1); await flow.observe();
  await until(() => f.journal.callCommand(id, 'call.promptVoice', 1)?.phase === 'result');
  assert.deepEqual(f.prompts.map(value => value.prompt), [
    'Begruesse den User mit "Servus"', 'Begruesse den User mit "Servus"',
  ]);
});

for (const code of ['app_tools_unavailable', 'phone_voice_not_ready']) test('failed greeting keeps live Voice and model selection: ' + code, async t => {
  const f = fixture(t), id = f.incoming(); f.failPrompt(true, code);
  const flow = f.flow();
  await flow.accept('main', randomUUID(), id);
  await until(() => f.errors.includes(code));
  await tick();
  assert.equal(f.journal.currentCall(id)?.callId, id);
  assert.ok(!f.methods().includes('call.hangup'));
  f.failSelection(true);
  f.command('select_voice', 1, 'sol', 'high'); await flow.observe();
  await until(() => f.journal.feedbackCommand(id, 1)?.phase === 'result');
  assert.equal(f.requests.find(r => r.method === 'call.command.feedback')?.params['success'], false);
  assert.deepEqual(flow.voiceSelection(id), { model: 'gpt-6-sol', reasoningEffort: 'high' });
  const status = await new PhoneBridge(flow, f.native).invoke('status', {}, context()) as { voiceSelection: unknown };
  assert.deepEqual(status.voiceSelection, { model: 'gpt-6-sol', reasoningEffort: 'high' });
  assert.equal(f.journal.currentCall(id)?.callId, id);
  await flow.hangup('main', id);
});

test('incoming greeting survives a transient invalid status immediately after answer', async t => {
  const f = fixture(t), id = f.incoming(), original = f.native.observe.bind(f.native);
  let rejected = false;
  t.mock.method(f.native, 'observe', async (callId?: string) => {
    if (!rejected && callId === id && f.journal.callCommand(id, 'call.answer')?.phase === 'result') {
      rejected = true;
      throw new IvyError('invalid_arguments', 'Transient native status did not satisfy its contract.');
    }
    return original(callId);
  });
  const flow = f.flow({ ...settings, incomingInitialPrompt: 'Begruesse den User mit "Servus"' });
  await flow.accept('main', randomUUID(), id);
  const deadline = Date.now() + 3000;
  while (f.journal.callCommand(id, 'call.promptVoice')?.phase !== 'result' && Date.now() < deadline)
    await new Promise(resolve => setTimeout(resolve, 10));
  assert.equal(f.journal.callCommand(id, 'call.promptVoice')?.phase, 'result');
  assert.equal(rejected, true);
  assert.deepEqual(f.prompts.map(value => value.prompt), ['Begruesse den User mit "Servus"']);
  assert.equal(f.methods().includes('call.hangup'), false);
  assert.deepEqual(f.errors, []);
});

test('incoming audio and Voice task preparation run together after answering with local waiting audio', async t => {
  const f = fixture(t), id = f.incoming(), audioGate = deferred(), taskGate = deferred();
  let taskStarted = false, prepares = 0;
  f.intercept(async request => {
    if (request.method === 'call.audio.prepare') await audioGate.promise;
  });
  const flow = f.flow(settings, undefined, {
    async prepare() { prepares++; taskStarted = true; await taskGate.promise; return null; },
    async bind() { throw new IvyError('phone_voice_task_missing', 'End the fixture after Voice start.'); },
    async prompt() {}, async select() {}, async release() {},
  });
  await flow.accept('main', randomUUID(), id);
  try {
    await until(() => taskStarted && f.methods().includes('call.audio.prepare'));
    assert.equal(f.journal.callCommand(id, 'call.answer')?.phase, 'result');
    assert.equal(f.journal.callCommand(id, 'call.waiting.end'), null);
  } finally { audioGate.resolve(); taskGate.resolve(); }
  await until(() => f.methods().includes('call.desktop.startVoice'));
  assert.equal(prepares, 1, 'a prepared initial chat has no target ID and needs no second preparation');
});

test('Phone flow releases an answered incoming call when Voice cannot bind to its prepared task', async t => {
  const f = fixture(t), gate = deferred();
  const flow = f.flow(settings, undefined, {
    async prepare() { return randomUUID(); },
    async bind() {
      await gate.promise;
      throw new IvyError('phone_voice_task_missing', 'Desktop did not persist a discoverable Voice task.');
    },
    async prompt() {},
    async select() {},
    async release() {},
  });
  const id = f.incoming(), call = await flow.accept('main', randomUUID(), id);
  await until(() => f.methods().includes('call.desktop.startVoice'));
  assert.ok(f.methods().includes('call.answer'));
  gate.resolve();
  await until(() => f.journal.currentCall(id) === null);
  assert.equal(call.callId, id);
  assert.ok(f.errors.includes('phone_voice_task_missing'));
  assert.ok(!f.methods().includes('call.waiting.end'));
  assert.ok(f.methods().includes('call.hangup') && f.methods().includes('call.release'));
});

test('Phone flow observes an already starting Desktop without a second activation', async t => {
  for (const phase of ['waiting', 'submitted']) {
    const f = fixture(t); f.launch(phase); const flow = f.flow(), call = await flow.request('main', randomUUID(), 'personal');
    await until(() => !!f.journal.callCommand(call.callId, 'call.dial')?.receipt);
    assert.equal(f.methods().filter(m => m === 'call.desktop.launch').length, 1);
    assert.equal(f.methods().filter(m => m === 'desktop.observe').length, 1); assert.deepEqual(f.errors, []);
  }
});

test('a refused Desktop activation waits for a concurrent startup without resubmitting', async t => {
  const f = fixture(t); f.launch('not_submitted'); f.desktopWaiting(2);
  const flow = f.flow(), call = await flow.request('main', randomUUID(), 'personal');
  await until(() => f.journal.callCommand(call.callId, 'call.dial')?.phase === 'result');
  assert.equal(f.methods().filter(method => method === 'call.desktop.launch').length, 1);
  assert.equal(f.methods().filter(method => method === 'desktop.observe').length, 3);
  await flow.hangup('main', call.callId);
});

test('Phone flow unknown Voice start confirms absent capture before releasing its slot', async t => {
  const f = fixture(t); f.voice('outcome_unknown', 'outcome_unknown'); const flow = f.flow(), operation = randomUUID();
  const call = await flow.request('main', operation, 'personal');
  await until(() => !!f.journal.callCommand(call.callId, 'call.release')?.receipt); await tick();
  assert.equal(f.methods().filter(method => method === 'call.dial').length, 1);
  assert.ok(f.methods().includes('call.hangup') && f.methods().includes('desktop.captureOwner') && f.methods().includes('call.release'));
  assert.equal(f.journal.currentCall(), null);
});

test('Phone flow retries an uncertain decline cleanup until Desktop capture is absent', async t => {
  const f = fixture(t); f.voice('outcome_unknown', 'outcome_unknown'); f.capture('matched'); const flow = f.flow();
  const call = await flow.request('main', randomUUID(), 'personal');
  await until(() => f.errors.includes('phone_voice_cleanup_unknown'));
  assert.equal(f.journal.currentCall()?.callId, call.callId);
  assert.ok(f.errors.includes('phone_voice_cleanup_unknown'));
  f.capture('none'); await flow.observe();
  await until(() => !!f.journal.callCommand(call.callId, 'call.release')?.receipt);
  assert.equal(f.journal.currentCall(), null);
});

test('Phone hangup cancels pending original Voice start without redial or target replacement', async t => {
  const f = fixture(t), gate = deferred(), flow = f.flow();
  f.intercept(async r => { if (r.method === 'call.desktop.startVoice') await gate.promise; });
  const call = await flow.request('main', randomUUID(), 'personal');
  await until(() => f.methods().includes('call.desktop.startVoice'));
  const stopping = flow.hangup('main', call.callId);
  await until(() => f.methods().includes('call.desktop.stopVoice'));
  gate.resolve(); await stopping; await tick();
  assert.equal(f.methods().filter(method => method === 'call.dial').length, 1); assert.equal(f.journal.currentCall(), null);
  assert.equal(f.methods().filter(m => m === 'call.desktop.startVoice').length, 1);
});

test('Phone close drains an admission already observing native status and sends no positive call command', async t => {
  const f = fixture(t), gate = deferred(), flow = f.flow();
  f.intercept(async r => { if (r.method === 'status') await gate.promise; });
  const pending = flow.request('main', randomUUID(), 'personal');
  const failed = assert.rejects(pending, { code: 'service_not_ready' });
  await until(() => f.methods().includes('status'));
  const closed = flow.close(); assert.equal(flow.close(), closed); gate.resolve();
  await failed; await closed;
  assert.deepEqual(f.methods(), ['status']); assert.equal(f.journal.currentCall(), null);
});

test('Phone close drains a cancelled MCP Voice command and closes App Tools', async t => {
  const f = fixture(t), gate = deferred(), flow = f.flow(), bridge = new PhoneBridge(flow, f.native);
  const closePool = t.mock.method(PhoneAppToolsPool.prototype, 'close');
  const call = await flow.request('main', randomUUID(), 'personal', 'voice');
  await until(() => f.journal.callCommand(call.callId, 'call.promptVoice')?.phase === 'result'); await tick();
  const preparesBeforeRestart = f.voicePrepares();
  let reads = 0;
  f.intercept(async r => { if (r.method === 'status' && reads++ === 0) await gate.promise; });
  const pending = bridge.invoke('restartVoice', { callId: call.callId, operationId: randomUUID() }, context());
  const cancelled = assert.rejects(pending, { code: 'phone_call_cancelled' });
  await until(() => reads > 0);
  const stopping = flow.close();
  try {
    await until(() => f.journal.currentCall(call.callId) === null); await tick();
    assert.equal(closePool.mock.callCount(), 0, 'App Tools stay open while the command settles');
  } finally { gate.resolve(); }
  await cancelled;
  await stopping;
  assert.equal(closePool.mock.callCount(), 1, 'cancelled controls cannot bypass pool cleanup');
  assert.equal(f.voicePrepares(), preparesBeforeRestart);
});

test('Phone target capacity failure precedes audio and Voice and preserves original cleanup receipts', async t => {
  // Model storage quota loss after preflight, before target retention. No Voice may open.
  const f = fixture(t); const flow = f.flow();
  f.intercept(async r => {
    if (r.method === 'call.desktop.launch') f.journal.limits.maxBytes = f.journal.status().reservedBytes;
  });
  const call = await flow.request('main', randomUUID(), 'personal');
  await until(() => f.errors.includes('phone_journal_capacity'));
  assert.equal(f.journal.target(call.callId), null);
  assert.ok(!f.methods().includes('call.audio.prepare') && !f.methods().includes('call.desktop.startVoice'));
  f.journal.limits.maxBytes += phoneOutcomeReservation * 3;
  await flow.hangup('main', call.callId); assert.equal(f.journal.currentCall(), null);
});

test('Phone flow refuses insufficient complete-call capacity before creating a native call', async t => {
  const f = fixture(t, phoneOutcomeReservation * 3), flow = f.flow();
  await assert.rejects(flow.request('main', randomUUID(), 'personal'), { code: 'phone_journal_capacity' });
  assert.deepEqual(f.methods(), ['status']); assert.equal(f.journal.currentCall(), null);
  assert.equal(f.journal.status().calls, 0);
});

test('Phone admits an arbitrary authenticated principal while retired native epochs cannot replay effects', async t => {
  const f = fixture(t), flow = f.flow();
  const operation = randomUUID(), call = await f.calls.admitOutgoing('stranger', operation, 'personal', 'voice');
  f.journal.loseEpoch(f.epoch); f.journal.beginEpoch(randomUUID());
  assert.deepEqual(await flow.request('stranger', operation, 'personal'), call); await tick();
  assert.deepEqual(f.methods(), ['status']);
  await flow.hangup('another-caller', call.callId); assert.equal(f.journal.currentCall(call.callId), null);
});

test('Phone targets survive epoch retirement and reopen, reject replacement and reserve exactly once', async t => {
  const f = fixture(t), call = await f.calls.admitOutgoing('main', randomUUID(), 'personal');
  const value = { callId: call.callId, desktop: structuredClone(desktop), audio: structuredClone(settings.audio), voiceInput: structuredClone(settings.voiceInput), voiceArchive: null };
  const before = f.journal.status().reservedBytes, saved = f.journal.retainTarget(value), usage = f.journal.status();
  assert.equal(usage.reservedBytes - before, phoneOutcomeReservation);
  assert.deepEqual(f.journal.retainTarget(value), saved); assert.deepEqual(f.journal.status(), usage);
  value.desktop.pid = 999; (saved.voiceInput['hotkeyFallback'] as Record<string, unknown>)['focusBeforeHotkey'] = false;
  assert.deepEqual(f.journal.target(call.callId)?.desktop, desktop);
  assert.deepEqual(f.journal.target(call.callId)?.voiceInput, settings.voiceInput);
  assert.throws(() => f.journal.retainTarget(value), { code: 'mutation_conflict' });
  assert.throws(() => f.journal.retainTarget({ ...value, callId: randomUUID() }), { code: 'phone_call_conflict' });
  f.native.close(); f.journal.loseEpoch(f.epoch); f.journal.close();
  const reopened = new PhoneJournal(f.root, { hostId: 'fixture', serviceNodeId: 'phone' });
  try {
    reopened.beginEpoch(randomUUID()); assert.deepEqual(reopened.target(call.callId)?.desktop, desktop);
    assert.equal(reopened.status().reservedBytes, 0); assert.ok(reopened.archiveStatus().records > 0);
    assert.equal(reopened.currentCall(), null);
    assert.throws(() => reopened.retainTarget(value), { code: 'mutation_conflict' });
  } finally { reopened.close(); }
});

const context = (caller = 'main'): InvocationContext => ({ callerPrincipalId: caller, generation: 1, signal: new AbortController().signal });
test('loopback probe is trusted idle inspection and creates no retained audio or command', async t => {
  const f = fixture(t), flow = f.flow(), bridge = new PhoneBridge(flow, f.native);
  const report = await bridge.invoke('probeLoopback', {}, context('arbitrary-host.service')) as { state: string; detected: boolean };
  assert.equal(report.state, 'completed'); assert.equal(report.detected, false); assert.equal(f.journal.status().operations, 0);
  const call = await flow.request('main', randomUUID(), 'personal', 'windows');
  await until(() => !!f.journal.callCommand(call.callId, 'call.windows.connect')?.receipt);
  await assert.rejects(bridge.invoke('probeLoopback', {}, context('admin')), { code: 'phone_call_busy' });
  assert.equal(f.methods().filter(method => method === 'audio.probe').length, 1);
});
test('audio setup inspection is available to every trusted caller, reports missing endpoints and has no effects', async t => {
  const f = fixture(t), bridge = new PhoneBridge(f.flow(), f.native);
  const result = await bridge.invoke('audioSetup', {}, context('arbitrary-host.service')) as { routes: { route: string; ready: boolean; endpoints: { state: string }[] }[] };
  assert.deepEqual(result.routes.map(route => route.route), ['voice', 'windows']);
  assert.ok(result.routes.every(route => !route.ready && route.endpoints.some(endpoint => endpoint.state === 'missing')));
  assert.deepEqual(f.methods(), ['inventory']); assert.equal(f.journal.status().operations, 0);
});
test('codec diagnostics preserve missing-library evidence and require an idle service', async t => {
  const f = fixture(t), flow = f.flow(), bridge = new PhoneBridge(flow, f.native);
  const report = await bridge.invoke('codecTest', {}, context('arbitrary-host.service')) as { passed: boolean; codecs: { name: string; error: string | null }[] };
  assert.equal(report.passed, false);
  assert.equal(report.codecs.find(codec => codec.name === 'EVS')?.error, 'codec_library_missing');
  assert.equal(f.journal.status().operations, 0);
  const call = await flow.request('main', randomUUID(), 'personal', 'windows');
  await until(() => !!f.journal.callCommand(call.callId, 'call.windows.connect')?.receipt);
  await assert.rejects(bridge.invoke('codecTest', {}, context('admin')), { code: 'phone_call_busy' });
  assert.equal(f.methods().filter(method => method === 'codec.test').length, 1);
});
test('archive reconciliation routes the exact generation through Hive validation and original-call authorization', async t => {
  const f = fixture(t), reconciled: Array<[string, number]> = [];
  const flow = f.flow(settings, { async run() {}, async reconcile(callId, generation = 0) { reconciled.push([callId, generation]); } });
  const bridge = new PhoneBridge(flow, f.native);
  const call = await flow.request('main', randomUUID(), 'personal', 'windows');
  await until(() => !!f.journal.callCommand(call.callId, 'call.windows.connect')?.receipt);
  assert.equal(await bridge.invoke('reconcileArchive', { callId: call.callId, generation: 2 }, context()), null);
  assert.equal(await bridge.invoke('reconcileArchive', { callId: call.callId }, context()), null);
  assert.equal(await bridge.invoke('reconcileArchive', { callId: call.callId, generation: 2 }, context('foreign')), null);
  await assert.rejects(bridge.invoke('reconcileArchive', { callId: call.callId, generation: 129 }, context()));
  await assert.rejects(bridge.invoke('reconcileArchive', { callId: call.callId, generation: 0.5 }, context()));
  assert.deepEqual(reconciled, [[call.callId, 2], [call.callId, 0], [call.callId, 2]]);
});
test('status exposes all concurrent calls to every trusted principal', async t => {
  const f = fixture(t, 512 * 1024 * 1024, 2), bridge = new PhoneBridge(f.flow(), f.native);
  const policy = new PhoneAdmission({ incoming: [], recipients: [
    { id: 'other', destination: 'sip:other@fixture' },
    { id: 'personal', destination: 'sip:personal@127.0.0.1' },
  ] });
  const first = f.journal.admitCall(policy.outgoing(f.epoch, randomUUID(), 'other', 'other', 'windows'));
  const second = f.journal.admitCall(policy.outgoing(f.epoch, randomUUID(), 'main', 'personal', 'windows'));
  const status = await bridge.invoke('status', {}, context()) as { call: { callId: string }; calls: { callId: string }[]; voiceSelection: unknown };
  assert.equal(status.call.callId, first.callId); assert.deepEqual(status.calls.map(call => call.callId), [first.callId, second.callId]);
  assert.equal(status.voiceSelection, null, 'Windows audio calls have no Voice model');
  const admin = await bridge.invoke('calls', {}, context('admin')) as { calls: { callId: string }[] };
  assert.deepEqual(admin.calls.map(call => call.callId), [first.callId, second.callId]);
  assert.ok(f.methods().every(method => method === 'status'));
});
test('audio inventory is available to every trusted caller and remains read-only without call admission', async t => {
  const f = fixture(t), bridge = new PhoneBridge(f.flow(), f.native);
  const inventory = await bridge.invoke('inventory', {}, context('arbitrary-host.service')) as { codecs: { name: string }[] };
  assert.equal(inventory.codecs[0]?.name, 'G722'); assert.deepEqual(f.methods(), ['inventory']);
  assert.equal(f.journal.status().operations, 0); assert.equal(f.credentialReads(), 0);
});
test('Phone status exposes native registration without dialing or reading credentials', async t => {
  const f = fixture(t), bridge = new PhoneBridge(f.flow(), f.native);
  for (const registration of [null, { state: 'registering', responseCode: null, remoteRemoved: null },
    { state: 'registered', responseCode: 200, remoteRemoved: false }, { state: 'failed', responseCode: 403, remoteRemoved: null }]) {
    f.registration(registration);
    const status = await bridge.invoke('status', {}, context()) as { registration: unknown; busy: boolean };
    assert.deepEqual(status.registration, registration); assert.equal(status.busy, false);
  }
  assert.ok(f.methods().every(method => method === 'status'));
  assert.equal(f.credentialReads(), 0); assert.equal(f.journal.currentCall(), null);
});

test('Phone public tools reject caller-supplied identity and retain operation results for every trusted caller', async t => {
  const f = fixture(t), bridge = new PhoneBridge(f.flow(), f.native);
  validateShared('RegistrySync', phoneRegistry());
  assert.ok(await bridge.invoke('status', {}, context('stranger')));
  const requestsBeforeInvalidInput = f.requests.length;
  for (const [key, value] of Object.entries({ principalId: 'main', destination: 'sip:other@127.0.0.1', desktop, password: 'secret' }))
    await assert.rejects(bridge.invoke('request', { operationId: randomUUID(), recipientId: 'personal', [key]: value }, context()));
  assert.equal(f.requests.length, requestsBeforeInvalidInput);
  const call = await bridge.invoke('request', { operationId: randomUUID(), recipientId: 'personal' }, context()) as { callId: string };
  await until(() => !!f.journal.callCommand(call.callId, 'call.dial')?.receipt); await tick();
  const receipt = await bridge.invoke('operation', { callId: call.callId, method: 'call.dial' }, context());
  assert.deepEqual(receipt, f.journal.callCommand(call.callId, 'call.dial'));
  const before = f.requests.length;
  assert.deepEqual(await bridge.invoke('operation', { callId: call.callId, method: 'call.dial' }, context('stranger')), receipt);
  assert.equal(f.requests.length, before);
  await bridge.invoke('hangup', { callId: call.callId }, context('admin'));
  const view = await bridge.invoke('call', { callId: call.callId }, context()) as { current: boolean; observation: unknown };
  assert.equal(view.current, false); assert.equal(view.observation, null);
});

test('Phone public status exposes calls while current policy still controls new effects after revocation', async t => {
  const f = fixture(t), originalFlow = f.flow(), call = await originalFlow.request('main', randomUUID(), 'personal');
  await until(() => !!f.journal.callCommand(call.callId, 'call.dial')?.receipt); await tick();
  const changedPolicy = new PhoneAdmission({
    recipients: [{ id: 'another', destination: 'sip:another@127.0.0.1' }], incoming: [] });
  const changedFlow = new PhoneFlow(new PhoneCallCommands(changedPolicy, f.journal, f.native), f.native, settings,
    async () => ({ username: null, password: null }));
  const bridge = new PhoneBridge(changedFlow, f.native);
  try {
    const status = await bridge.invoke('status', {}, context('other')) as { busy: boolean; call: unknown; incoming: unknown; recipients: string[] };
    assert.equal(status.busy, true); assert.deepEqual(status.call, call); assert.equal(status.incoming, null);
    assert.deepEqual(status.recipients, ['another']);
    await assert.rejects(bridge.invoke('request', { operationId: randomUUID(), recipientId: 'personal' }, context()), { code: 'phone_destination_refused' });
    assert.ok(await bridge.invoke('operation', { callId: call.callId, method: 'call.dial' }, context()));
    await bridge.invoke('hangup', { callId: call.callId }, context()); assert.equal(f.journal.currentCall(), null);
  } finally { await changedFlow.close(); }
});

test('Phone service contracts bind explicit configuration, protected credential path and incoming principal', () => {
  const value = { ...settings, native: { executable: 'dist/native/phone/Ivy.PhoneRuntime.exe', executableHash: 'sha256:' + 'a'.repeat(64) },
    binding: { address: '127.0.0.1', port: 0, transport: 'udp' }, codecs: { preferences: ['G722'], packetMs: 20 },
    registration: null, credentialsPath: null, incomingPrincipalId: null, pollMs: 500,
    policy: { recipients: [], incoming: [] } };
  assert.deepEqual(phoneSettings(value), value);
  const preferred = phoneSettings({ ...value, voiceInput: { micro: settings.voiceInput.micro } });
  assert.equal(preferred.voiceInput['hotkeyFallback'], null, 'omitting fallback never enables keyboard input');
  const hotkeyOnly = phoneSettings({ ...value, voiceInput: { micro: null, hotkeyFallback: settings.voiceInput.hotkeyFallback } });
  assert.equal(hotkeyOnly.voiceInput['micro'], null, 'an explicit Codex Voice hotkey can own Voice routing without Micro');
  assert.throws(() => phoneSettings({ ...value, voiceInput: { micro: null, hotkeyFallback: null } }),
    { code: 'invalid_arguments' });
  assert.throws(() => phoneSettings({ ...value, voiceInput: { micro: null } }),
    { code: 'invalid_arguments' });
  assert.throws(() => phoneSettings({ ...value, voiceInput: { micro: { ...settings.voiceInput.micro, usbipExecutable: 'usbip.exe' } } }),
    { code: 'invalid_arguments' });
  assert.throws(() => phoneSettings({ ...value, voiceInput: { ...settings.voiceInput, hotkeyFallback: true } }));
  assert.throws(() => phoneSettings({ ...value, credentialsPath: 'relative.json' }), { code: 'invalid_arguments' });
  assert.throws(() => phoneSettings({ ...value, incomingPrincipalId: 'unconfigured' }), { code: 'invalid_arguments' });
  assert.throws(() => phoneSettings({ ...value, incomingInitialPrompt: '   ' }), { code: 'invalid_arguments' });
  assert.throws(() => phoneSettings({ ...value, native: { ...value.native, executable: 'arbitrary.exe' } }));
  assert.throws(() => validatePhoneService('PhoneCredentials', { username: 'u', password: 'p', principalId: 'main' }));
});

test('default outgoing Windows route and retained history are visible to every trusted principal', async t => {
  const { outgoingRoute: _, ...defaults } = settings;
  const f = fixture(t), flow = f.flow(defaults), bridge = new PhoneBridge(flow, f.native);
  const call = await flow.request('main', randomUUID(), 'personal');
  await until(() => !!f.journal.callCommand(call.callId, 'call.windows.connect')?.receipt);
  assert.equal(call.route, 'windows'); assert.ok(!f.methods().includes('call.desktop.startVoice'));
  await flow.hangup('main', call.callId);
  const mine = await bridge.invoke('history', {}, context()) as { calls: { callId: string }[] };
  assert.deepEqual(mine.calls.map(c => c.callId), [call.callId]);
  const other = await bridge.invoke('history', {}, context('other')) as { calls: { callId: string }[] };
  assert.deepEqual(other.calls.map(c => c.callId), [call.callId]);
  const admin = await bridge.invoke('history', {}, context('admin')) as { calls: { callId: string }[] };
  assert.deepEqual(admin.calls.map(c => c.callId), [call.callId]);
});
