import test from 'node:test';
import type { TestContext } from 'node:test';
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { PassThrough } from 'node:stream';
import { PhoneAdmission } from '../services/phone-bridge/src/runtime/admission.js';
import { PhoneCallCommands } from '../services/phone-bridge/src/runtime/calls.js';
import { PhoneJournal } from '../services/phone-bridge/src/runtime/journal.js';
import { PhoneNativeClient, validatePhone } from '../services/phone-bridge/src/runtime/native.js';

const tick = () => new Promise<void>(done => setImmediate(done));
test('Phone Voice lifetime receipts distinguish observation from a submitted or uncertain chord', () => {
  const dispatch = { phase: 'submitted', method: 'hotkey', micro: null, hotkey: { phase: 'submitted', requested: 6, submitted: 6, keyUpSubmitted: false, errorCode: null } };
  validatePhone('VoiceLifetimeResult', { state: 'active', dispatch, reason: null });
  validatePhone('VoiceLifetimeResult', { state: 'active', dispatch: null, reason: null });
  validatePhone('VoiceLifetimeResult', { state: 'stopped', dispatch: null, reason: null });
  validatePhone('VoiceLifetimeResult', { state: 'not_started', dispatch: null, reason: 'baseline_stale' });
  for (const invalid of [
    { state: 'active', dispatch: { ...dispatch, phase: 'outcome_unknown' } },
    { state: 'not_started', dispatch }, { state: 'submitted', dispatch }
  ]) assert.throws(() => validatePhone('VoiceLifetimeResult', { ...invalid, reason: null }));
  assert.throws(() => validatePhone('VoiceLifetimeResult', { state: 'not_started', dispatch: null, reason: 'raw exception text' }));
  assert.throws(() => validatePhone('VoiceLifetimeResult', { state: 'active', dispatch, reason: 'baseline_stale' }));
});
const idle = { configured: true, endpoint: '127.0.0.1:5060', call: null, registration: null, media: null };
function ringing() {
  const id = randomUUID(), incoming = { sipCallId: 'observed-wire-call', fromUri: 'sip:known@fixture', peerAddress: '127.0.0.1', peerPort: 5060, transport: 'udp' };
  return { ...idle, call: { id, direction: 'incoming', state: 'ringing', sipCallId: incoming.sipCallId, error: null, incoming },
    media: { closed: false, failed: false, sentPackets: 0, receivedPackets: 0,
      receive: { queuedPackets: 0, maximumPackets: 8, reorderMs: 20, duplicatePackets: 0, latePackets: 0, overflowPackets: 0,
        foreignPackets: 0, gatedPackets: 0, missingPackets: 0, maximumResidenceMs: 0 }, audio: { callId: id, state: 'unprepared', route: null } } };
}
function fixture(t: TestContext) {
  const root = mkdtempSync(join(tmpdir(), 'ivy-phone-calls-'));
  const policy = new PhoneAdmission({
    recipients: [{ id: 'personal', destination: 'sip:personal@127.0.0.1' }],
    incoming: [{ peerAddress: '127.0.0.1', transport: 'udp', fromUri: 'sip:known@fixture' }] });
  const journal = new PhoneJournal(root, { hostId: 'fixture', serviceNodeId: 'phone' }), epoch = randomUUID(); journal.beginEpoch(epoch);
  const input = new PassThrough(), output = new PassThrough(), requests: Record<string, unknown>[] = [];
  input.on('data', bytes => { requests.push(JSON.parse(String(bytes).trim()) as Record<string, unknown>); });
  const native = new PhoneNativeClient(epoch, input, output, journal.hooks), calls = new PhoneCallCommands(policy, journal, native);
  const admit = () => journal.admitCall(policy.outgoing(epoch, randomUUID(), 'main', 'personal'));
  const reply = (request: Record<string, unknown>, result: unknown, error: string | null = null) => {
    output.write(JSON.stringify({ version: 1, epoch, requestId: request['requestId'], ok: error === null, result, error }) + '\n');
  };
  t.after(() => { native.close(); input.destroy(); output.destroy(); journal.close();
    assert.equal(dirname(resolve(root)), resolve(tmpdir())); assert.ok(basename(root).startsWith('ivy-phone-calls-'));
    rmSync(root, { recursive: true, force: true }); });
  return { policy, journal, epoch, native, calls, requests, reply, admit, output };
}

test('Phone commands share original concurrent dispatch and saved receipts without replacing call destination', async t => {
  const f = fixture(t), call = f.admit(), first = f.calls.prepare('main', call.callId), again = f.calls.prepare('main', call.callId);
  assert.equal(first, again); assert.equal(f.journal.status().operations, 1);
  await tick(); assert.equal(f.requests.length, 1); f.reply(f.requests[0]!, { callId: call.callId });
  const prepared = await first; assert.deepEqual(await f.calls.prepare('main', call.callId), prepared);
  const credentials = { username: 'fixture', password: 'fixture-secret' };
  const dial = f.calls.dial('main', call.callId, credentials, 30);
  assert.equal(f.calls.dial('main', call.callId, credentials, 30), dial);
  assert.throws(() => f.calls.dial('main', call.callId, credentials, 31), { code: 'mutation_conflict' });
  await tick(); assert.equal(f.requests.length, 2);
  assert.deepEqual(f.requests[1]!['params'], { callId: call.callId, destination: call.destination, ...credentials, ringSeconds: 30, waiting: false });
  f.reply(f.requests[1]!, { callId: call.callId, state: 'connected' }); const connected = await dial;
  const recreated = new PhoneCallCommands(f.policy, f.journal, f.native);
  assert.deepEqual(await recreated.dial('main', call.callId, credentials, 30), connected);
  assert.equal(JSON.stringify(connected).includes(credentials.password), false); assert.equal(f.requests.length, 2);
});

test('Phone lost pipe keeps unknown original command and slot; neither another adapter nor owner can replay it', async t => {
  const f = fixture(t), call = f.admit(), prepare = f.calls.prepare('main', call.callId);
  const failed = assert.rejects(prepare, { code: 'phone_pipe_closed', outcome: 'unknown' });
  await tick(); f.output.end(); await failed;
  const recreated = new PhoneCallCommands(f.policy, f.journal, f.native);
  assert.throws(() => recreated.prepare('main', call.callId), { code: 'phone_operation_retained', outcome: 'unknown' });
  assert.throws(() => recreated.cancelUnprepared('admin', call.callId), { code: 'phone_release_unconfirmed' });
  assert.throws(f.admit, { code: 'phone_call_busy' });
  f.journal.loseEpoch(f.epoch); f.journal.beginEpoch(randomUUID());
  assert.equal(recreated.read('main', call.callId, 'call.prepare')!.phase, 'outcome_unknown');
  assert.throws(() => recreated.prepare('main', call.callId), { code: 'phone_operation_retained', outcome: 'unknown' });
  assert.throws(() => recreated.hangup('admin', call.callId), { code: 'phone_call_conflict' });
  assert.equal(f.requests.length, 1);
});

test('Phone trusts every authenticated caller while policy changes still block new connect effects', async t => {
  const f = fixture(t), call = f.admit();
  assert.equal(f.calls.read('stranger', call.callId, 'call.prepare'), null);
  const changed = new PhoneAdmission({ recipients: [], incoming: [] });
  const commands = new PhoneCallCommands(changed, f.journal, f.native);
  assert.throws(() => commands.prepare('main', call.callId), { code: 'phone_policy_changed' });
  assert.equal(f.requests.length, 0); assert.equal(f.journal.status().operations, 0);
  const prepare = f.calls.prepare('main', call.callId); await tick(); f.reply(f.requests[0]!, { callId: call.callId }); await prepare;
  assert.equal(commands.read('admin', call.callId, 'call.prepare')!.receipt!.ok, true);
  const release = commands.release('admin', call.callId); await tick(); assert.equal(f.journal.currentCall()!.callId, call.callId);
  f.reply(f.requests[1]!, { callId: call.callId, released: true }); const receipt = await release;
  assert.equal(f.journal.currentCall(), null); const next = f.admit();
  assert.deepEqual(await commands.release('admin', call.callId), receipt);
  assert.equal(f.journal.currentCall()!.callId, next.callId); assert.equal(f.requests.length, 2);
});

test('Phone slot needs the exact original release proof, not success on another call or merely hangup', async t => {
  const f = fixture(t), call = f.admit(), hangup = f.calls.hangup('main', call.callId);
  await tick(); f.reply(f.requests[0]!, { callId: call.callId, state: 'ended' }); await hangup;
  assert.equal(f.journal.currentCall()!.callId, call.callId);
  const release = f.calls.release('main', call.callId), failed = assert.rejects(release, { code: 'phone_release_unconfirmed' });
  await tick(); f.reply(f.requests[1]!, { callId: randomUUID(), released: true }); await failed;
  assert.equal(f.journal.currentCall()!.callId, call.callId);
  assert.throws(() => f.calls.release('main', call.callId), { code: 'phone_release_unconfirmed' });
  assert.equal(f.requests.length, 2); assert.equal(f.calls.read('main', call.callId, 'call.release')!.phase, 'result');
});

test('Phone can cancel only locally unprepared outgoing admissions, including explicit native prepare rejection', async t => {
  const f = fixture(t), local = f.admit(); f.calls.cancelUnprepared('main', local.callId);
  assert.equal(f.journal.currentCall(), null); assert.equal(f.requests.length, 0);
  assert.throws(() => f.calls.prepare('main', local.callId), { code: 'phone_call_conflict' });
  for (const error of ['operation_conflict', 'runtime_not_ready']) {
    const call = f.admit(); f.calls.cancelUnprepared('admin', local.callId); assert.equal(f.journal.currentCall()!.callId, call.callId);
    const prepare = f.calls.prepare('main', call.callId); await tick(); f.reply(f.requests.at(-1)!, null, error);
    assert.equal((await prepare).receipt!.error, error); assert.equal(f.journal.currentCall(), null);
    assert.equal((await f.calls.prepare('main', call.callId)).receipt!.error, error);
  }
  assert.equal(f.requests.length, 2);
  const call = f.admit(), prepare = f.calls.prepare('main', call.callId); await tick();
  f.reply(f.requests.at(-1)!, null, 'operation_outcome_unknown'); await prepare;
  assert.throws(() => f.calls.cancelUnprepared('main', call.callId), { code: 'phone_release_unconfirmed' });
  assert.equal(f.journal.currentCall()!.callId, call.callId);
});

test('Phone incoming calls retain direction and require actual native release after claiming', async t => {
  const f = fixture(t), id = randomUUID(), incoming = { sipCallId: 'wire-call', fromUri: 'sip:known@fixture',
    peerAddress: '127.0.0.1', peerPort: 5060, transport: 'udp' as const };
  const call = f.journal.admitCall(f.policy.incoming(f.epoch, randomUUID(), 'main', {
    id, direction: 'incoming', state: 'ringing', sipCallId: incoming.sipCallId, error: null, incoming }));
  assert.throws(() => f.calls.dial('main', id, { username: null, password: null }, 30), { code: 'phone_call_conflict' });
  await assert.rejects(f.calls.prepare('main', id), { code: 'phone_call_conflict' });
  assert.equal(f.requests.length, 0); assert.equal(f.journal.status().operations, 0);
  const claim = f.calls.claim('main', call.callId); await tick();
  assert.throws(() => f.calls.cancelUnprepared('main', id), { code: 'phone_release_unconfirmed' });
  f.reply(f.requests[0]!, { id, direction: 'incoming', state: 'ringing', sipCallId: incoming.sipCallId, error: null, incoming }); await claim;
  const answer = f.calls.answer('main', call.callId); await tick(); f.reply(f.requests[1]!, { callId: id, state: 'connected' }); await answer;
  assert.throws(() => f.calls.cancelUnprepared('main', id), { code: 'phone_release_unconfirmed' });
  assert.equal(f.journal.currentCall()!.callId, id);
});

test('Phone unclaimed incoming offers release only their local admission without sending a native command', t => {
  const f = fixture(t), id = randomUUID(), incoming = { sipCallId: 'wire-unclaimed', fromUri: 'sip:known@fixture',
    peerAddress: '127.0.0.1', peerPort: 5060, transport: 'udp' as const };
  const call = f.journal.admitCall(f.policy.incoming(f.epoch, randomUUID(), 'main', {
    id, direction: 'incoming', state: 'ringing', sipCallId: incoming.sipCallId, error: null, incoming }));
  f.calls.cancelUnprepared('main', id);
  assert.equal(f.journal.currentCall(), null); assert.deepEqual(f.journal.getCall(id), call);
  assert.equal(f.requests.length, 0); assert.equal(f.journal.status().operations, 0);
});

test('Phone outgoing admission uses original live native status and concurrent retries keep one allocation', async t => {
  const f = fixture(t), operationId = randomUUID();
  const first = f.calls.admitOutgoing('main', operationId, 'personal'), retry = f.calls.admitOutgoing('main', operationId, 'personal');
  await tick(); assert.equal(f.requests.length, 2); assert.ok(f.requests.every(request => request['method'] === 'status' && request['operationId'] === null));
  f.reply(f.requests[0]!, idle); const call = await first;
  // The first admission can progress while the second original status read is still pending.
  f.reply(f.requests[1]!, ringing()); assert.deepEqual(await retry, call);
  assert.equal(f.journal.status().calls, 1); assert.equal(f.journal.status().operations, 0);
  f.native.close(); f.journal.loseEpoch(f.epoch);
  assert.deepEqual(await f.calls.admitOutgoing('main', operationId, 'personal'), call);
  assert.equal(f.requests.length, 2);
});

test('Phone incoming admission binds the owner observation, never a caller context or a replacement native call', async t => {
  const f = fixture(t), observed = ringing(), operationId = randomUUID();
  const request = f.calls.admitIncoming('main', operationId, observed.call.id);
  await tick(); f.reply(f.requests[0]!, observed); const call = await request;
  assert.equal(call.callId, observed.call.id); assert.deepEqual(call.incoming, observed.call.incoming);
  f.native.close(); f.journal.loseEpoch(f.epoch);
  assert.deepEqual(await f.calls.admitIncoming('main', operationId, observed.call.id), call);
  await assert.rejects(f.calls.admitIncoming('main', operationId, randomUUID()), { code: 'mutation_conflict' });
  await assert.rejects(f.calls.admitIncoming('admin', operationId, observed.call.id), { code: 'mutation_conflict' });
  await assert.rejects(f.calls.admitIncoming('main', operationId, observed.call.id, 'windows'), { code: 'mutation_conflict' });
  assert.equal(f.requests.length, 1);
});

test('Phone live admission refuses stale epochs, busy native calls, changed calls and unapproved caller metadata', async t => {
  for (const failure of ['epoch', 'busy', 'changed', 'caller'] as const) {
    const f = fixture(t), observed = ringing(), operationId = randomUUID();
    const request = failure === 'changed' || failure === 'caller' ? f.calls.admitIncoming('main', operationId, observed.call.id) :
      f.calls.admitOutgoing('main', operationId, 'personal');
    const failed = assert.rejects(request, { code: failure === 'epoch' ? 'phone_epoch_mismatch' : failure === 'busy' ? 'phone_call_busy' :
      failure === 'changed' ? 'phone_call_conflict' : 'phone_caller_refused' });
    await tick();
    if (failure === 'epoch') f.journal.loseEpoch(f.epoch);
    if (failure === 'caller') observed.call.incoming.peerAddress = '127.0.0.2';
    f.reply(f.requests[0]!, failure === 'epoch' ? idle : failure === 'changed' ? ringing() : observed); await failed;
    assert.equal(f.journal.status().calls, 0); assert.equal(f.journal.status().operations, 0);
  }
});

test('Phone observed status rejects missing fields and mismatched native audio or wire-call identity', async t => {
  for (const failure of ['shape', 'audio', 'wire', 'media'] as const) {
    const f = fixture(t), status = ringing();
    const request = f.native.observe(), failed = assert.rejects(request);
    if (failure === 'audio') status.media.audio.callId = randomUUID();
    if (failure === 'wire') status.call.incoming.sipCallId = 'another-wire-call';
    await tick(); f.reply(f.requests[0]!, failure === 'shape' ? { configured: true } : failure === 'media' ? { ...status, media: null } : status);
    await failed; assert.equal(f.journal.status().operations, 0);
  }
});

test('Phone Desktop activation and configured focus use original call receipts without retrying uncertain hotkeys', async t => {
  const f = fixture(t), call = f.admit(), application = { appUserModelId: 'Fixture.Package!UI', startIfMissing: true };
  const launch = f.calls.launchDesktop('main', call.callId, application);
  assert.equal(f.calls.launchDesktop('main', call.callId, application), launch);
  await tick(); f.reply(f.requests[0]!, { callId: call.callId, action: 'launch', result: { phase: 'submitted', identity: null, activationPid: 123, errorCode: null } });
  const launched = await launch; assert.equal(launched.intent.method, 'call.desktop.launch');
  const desktop = { pid: 123, startTimeUtcTicks: '1', imagePath: 'C:\\fixture-desktop.exe', appUserModelId: application.appUserModelId };
  const voiceInput = { micro: { usbipExecutable: 'C:\\verified\\usbip.exe', executableHash: 'sha256:' + '0'.repeat(64), port: 3241 }, hotkeyFallback: { modifiers: ['control', 'shift'], keyCode: 32, focusBeforeHotkey: true } };
  const before = f.journal.status().operations;
  await assert.rejects(f.calls.startVoice('main', call.callId, desktop, { ...voiceInput, hotkeyFallback: { ...voiceInput.hotkeyFallback, focusBeforeHotkey: 'true' } }));
  assert.equal(f.journal.status().operations, before);
  const start = f.calls.startVoice('main', call.callId, desktop, voiceInput); await tick();
  assert.deepEqual(f.requests[1]!['params'], { callId: call.callId, desktop, voiceInput });
  f.reply(f.requests[1]!, { callId: call.callId, action: 'startVoice', result: { state: 'outcome_unknown', reason: 'input_outcome_unknown', dispatch: { phase: 'outcome_unknown', method: 'hotkey', micro: null, hotkey: { phase: 'outcome_unknown', requested: 6, submitted: 1, keyUpSubmitted: true, errorCode: 'partial_input' } } } });
  const started = await start; assert.deepEqual(await f.calls.startVoice('main', call.callId, desktop, voiceInput), started);
  assert.throws(() => f.journal.submit(started.intent), { code: 'phone_operation_retained', outcome: 'unknown' });
  assert.throws(() => f.journal.submit({ ...started.intent, operationId: randomUUID() }), { code: 'phone_call_conflict' });
  assert.equal(f.requests.length, 2);
  const changed = new PhoneCallCommands(new PhoneAdmission({ recipients: [], incoming: [] }), f.journal, f.native);
  assert.throws(() => changed.startVoice('main', call.callId, desktop, voiceInput), { code: 'phone_policy_changed' });
  const stop = changed.stopVoice('admin', call.callId, desktop, voiceInput); await tick();
  f.reply(f.requests[2]!, { callId: call.callId, action: 'stopVoice', result: { state: 'outcome_unknown', reason: null, dispatch: { phase: 'not_submitted', method: 'hotkey', micro: null, hotkey: { phase: 'not_submitted', requested: 6, submitted: 0, keyUpSubmitted: false, errorCode: 'desktop_focus_failed' } } } });
  const stopped = await stop; assert.deepEqual(await changed.stopVoice('admin', call.callId, desktop, voiceInput), stopped);
  assert.equal(f.requests.length, 3);
});

test('Phone Desktop reads and action receipts require exact application, call and action ownership', async t => {
  const f = fixture(t), application = 'Fixture.Package!UI';
  const observing = f.native.observeDesktop(application); await tick();
  assert.equal(f.requests[0]!['method'], 'desktop.observe'); assert.equal(f.requests[0]!['operationId'], null);
  f.reply(f.requests[0]!, { state: 'absent', identity: null }); assert.deepEqual(await observing, { state: 'absent', identity: null });
  assert.equal(f.journal.status().operations, 0);
  const wrong = f.native.observeDesktop(application), wrongRejected = assert.rejects(wrong, { code: 'phone_invalid_status' }); await tick();
  f.reply(f.requests[1]!, { state: 'ready', identity: { pid: 123, startTimeUtcTicks: '1', imagePath: 'C:\\fixture.exe', appUserModelId: 'Foreign.Package!UI' } }); await wrongRejected;
  const call = f.admit(), launch = f.calls.launchDesktop('main', call.callId, { appUserModelId: application, startIfMissing: false });
  const failed = assert.rejects(launch, { code: 'phone_receipt_conflict' }); await tick();
  f.reply(f.requests[2]!, { callId: randomUUID(), action: 'launch', result: { phase: 'waiting', identity: null, activationPid: null, errorCode: null } }); await failed;
  assert.throws(() => f.calls.launchDesktop('main', call.callId, { appUserModelId: application, startIfMissing: false }), { code: 'phone_receipt_conflict' });
  assert.equal(f.requests.length, 3);
});
