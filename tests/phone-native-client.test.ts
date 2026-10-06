import test from 'node:test';
import assert from 'node:assert/strict';
import { PassThrough } from 'node:stream';
import { randomUUID } from 'node:crypto';
import { PhoneNativeClient, phoneFrameBytes, validatePhone } from '../services/phone-bridge/src/runtime/native.js';
import type { PhoneReceiptHooks, PhoneReply } from '../services/phone-bridge/src/runtime/native.js';

function deferred() { let resolve!: () => void; const promise = new Promise<void>(done => { resolve = done; }); return { promise, resolve }; }
const tick = () => new Promise<void>(resolve => setImmediate(resolve));

test('Phone EVS is an explicit codec preference with bounded20ms configuration', () => {
  validatePhone('CodecSettings', { preferences: ['EVS', 'OPUS', 'G722', 'PCMA', 'PCMU'], packetMs: 20 });
  for (const preferences of [['EVS', 'EVS'], ['evs'], ['AMR-WB'], []])
    assert.throws(() => validatePhone('CodecSettings', { preferences, packetMs: 20 }));
  assert.throws(() => validatePhone('CodecSettings', { preferences: ['EVS'], packetMs: 40 }));
});

test('audio mute policy is explicit, bounded and retained in the original preparation request', async () => {
  const peer = fixture({ beforeSend: async () => undefined, beforeResolve: async () => undefined });
  const callId = randomUUID(), desktop = { pid: 123, startTimeUtcTicks: '1', imagePath: 'C:\\fixture.exe', appUserModelId: 'Fixture.Package!UI' };
  const settings = { captureEndpointId: 'capture', renderEndpointId: 'receive', sourceMode: 'desktop_process',
    captureBufferMs: 20, renderLatencyMs: 20, queueMs: 40,
    mutePolicy: { mutedRenderEndpointId: 'speaker', unmuteReceive: true, intervalSeconds: 60 } };
  try {
    const pending = peer.client.request('call.audio.prepare', { callId, desktop, settings }, randomUUID());
    await tick(); assert.deepEqual((peer.requests[0]!['params'] as { settings: unknown }).settings, settings);
    peer.reply(peer.requests[0]!); await pending;
    for (const mutePolicy of [{ intervalSeconds: 0 }, { intervalSeconds: 3601 }, { mutedRenderEndpointId: '' }, { useDefaultOutput: true }])
      assert.throws(() => validatePhone('AudioSettings', { ...settings, mutePolicy }));
    validatePhone('AudioSettings', { ...settings, mutePolicy: null });
  } finally { peer.close(); }
});

test('Phone SIP configuration admits initial, delayed and automatic outgoing offer modes', () => {
  const binding = { address: '127.0.0.1', port: 0, transport: 'udp' };
  validatePhone('SipBinding', binding);
  for (const outgoingOfferMode of ['initial', 'delayed', 'auto']) {
    validatePhone('Configure', { binding: { ...binding, outgoingOfferMode }, codecs: { preferences: ['PCMA'], packetMs: 20 },
      incomingPeers: [], registration: null, password: null });
  }
  for (const outgoingOfferMode of ['unknown', '', null, true]) {
    assert.throws(() => validatePhone('SipBinding', { ...binding, outgoingOfferMode }));
  }
});

test('Micro startup configuration and readiness travel through the native status contract', () => {
  validatePhone('Configure', { binding: { address: '127.0.0.1', port: 0, transport: 'udp' }, codecs: { preferences: ['PCMA'], packetMs: 20 },
    incomingPeers: ['registered'], registration: null, password: null,
    micro: { usbipExecutable: 'C:\\verified\\usbip.exe', executableHash: 'sha256:' + '0'.repeat(64), port: 3241 } });
  for (const micro of [null, { state: 'ready', generation: 4294967296, errorCode: null },
    { state: 'unavailable', generation: null, errorCode: 'micro_attachment_conflict' }])
    validatePhone('Status', { configured: true, endpoint: null, call: null, media: null, registration: null, micro });
});

function fixture(hooks: PhoneReceiptHooks) {
  const input = new PassThrough(), output = new PassThrough(), epoch = randomUUID();
  const requests: Record<string, unknown>[] = [];
  input.on('data', bytes => { requests.push(JSON.parse(String(bytes).trim()) as Record<string, unknown>); });
  const client = new PhoneNativeClient(epoch, input, output, hooks);
  const reply = (request: Record<string, unknown>, result: unknown = { accepted: true }) => output.write(JSON.stringify({ version: 1, epoch,
    requestId: request['requestId'], ok: true, result, error: null }) + '\n');
  return { client, input, output, requests, reply, close: () => { client.close(); input.destroy(); output.destroy(); } };
}

test('Phone Desktop activation is an explicit retained effect with its exact packaged application', async () => {
  const intents: unknown[] = [], receipts: unknown[] = [];
  const peer = fixture({ beforeSend: async value => { intents.push(value); }, beforeResolve: async (_intent, value) => { receipts.push(value); } });
  const application = { appUserModelId: 'Fixture.Package!UI', startIfMissing: true }, operationId = randomUUID();
  try {
    const pending = peer.client.launchDesktop(application, operationId); await tick();
    assert.equal(peer.requests[0]!.method, 'desktop.launch'); assert.equal(peer.requests[0]!.operationId, operationId);
    assert.deepEqual(peer.requests[0]!.params, application); assert.equal(intents.length, 1);
    const result = { phase: 'submitted', identity: null, activationPid: 123, errorCode: null };
    peer.reply(peer.requests[0]!, result); assert.deepEqual(await pending, result); assert.equal(receipts.length, 1);
  } finally { peer.close(); }
});

test('Phone scoped Desktop controls preserve visibility and unknown toggle without recording effects or retaining stale values', async () => {
  const peer = fixture({ beforeSend: async () => assert.fail('read-only'), beforeResolve: async () => assert.fail('read-only') });
  const desktop = { pid: 123, startTimeUtcTicks: '1', imagePath: 'C:\\fixture.exe', appUserModelId: 'Fixture.Package!UI' };
  try {
    const pending = peer.client.observeControls(desktop, ['Stop voice chat']); await tick();
    assert.equal(peer.requests[0]!['operationId'], null); assert.deepEqual(peer.requests[0]!['params'], { desktop, labels: ['Stop voice chat'] });
    const result = { state: 'observed', windows: [{ handle: '123', minimized: true, controls: [
      { id: [42, 1], parentId: [42, 0], name: 'Stop voice chat', enabled: false, offscreen: true, toggle: null }] }] };
    peer.reply(peer.requests[0]!, result); assert.deepEqual(await pending, result);
    const next = peer.client.observeControls(desktop, ['Stop voice chat']); await tick();
    peer.reply(peer.requests[1]!, { state: 'unavailable', windows: [] }); assert.deepEqual(await next, { state: 'unavailable', windows: [] });
    await assert.rejects(peer.client.observeControls(desktop, []));
    await assert.rejects(peer.client.observeControls(desktop, ['same', 'same'])); assert.equal(peer.requests.length, 2);
  } finally { peer.close(); }
});

test('Phone controls reject unrequested labels, partial unavailable data and duplicate window/element ownership', async () => {
  const desktop = { pid: 123, startTimeUtcTicks: '1', imagePath: 'C:\\fixture.exe', appUserModelId: 'Fixture.Package!UI' };
  for (const invalid of ['label', 'window', 'element', 'parent', 'unavailable', 'empty', 'toggle'] as const) {
    const peer = fixture({ beforeSend: async () => assert.fail('read-only'), beforeResolve: async () => assert.fail('read-only') });
    try {
      const pending = peer.client.observeControls(desktop, ['Stop voice chat']), rejected = assert.rejects(pending); await tick();
      const control = { id: [42, 1], parentId: invalid === 'parent' ? [42, 1] : [42, 0],
        name: invalid === 'label' ? 'unrequested text' : 'Stop voice chat', enabled: true, offscreen: false, toggle: invalid === 'toggle' ? 3 : null };
      const window = { handle: '123', minimized: false, controls: invalid === 'element' ? [control, control] : [control] };
      peer.reply(peer.requests[0]!, { state: invalid === 'unavailable' ? 'unavailable' : 'observed',
        windows: invalid === 'empty' ? [] : invalid === 'window' ? [window, window] : [window] }); await rejected;
    } finally { peer.close(); }
  }
});
test('Phone Desktop capture correlation retains exact session and process incarnation without cached match or effect receipts', async () => {
  const desktop = { pid: 123, startTimeUtcTicks: '1', imagePath: 'C:\\fixture.exe', appUserModelId: 'Fixture.Package!UI' };
  const peer = fixture({ beforeSend: async () => assert.fail('read-only'), beforeResolve: async () => assert.fail('read-only') });
  try {
    const matched = peer.client.observeDesktopCapture(desktop, 'capture'); await tick();
    assert.equal(peer.requests[0]!.method, 'desktop.captureOwner');
    assert.deepEqual(peer.requests[0]!.params, { desktop, endpointId: 'capture' });
    const result = { endpointId: 'capture', state: 'matched', identity: { instanceId: 'session', pid: 456, processStartTimeUtcTicks: '123456' } };
    peer.reply(peer.requests[0]!, result); assert.deepEqual(await matched, result);
    for (const state of ['none', 'ambiguous', 'unavailable']) {
      const pending = peer.client.observeDesktopCapture(desktop, 'capture'); await tick();
      peer.reply(peer.requests.at(-1)!, { endpointId: 'capture', state, identity: null });
      assert.deepEqual(await pending, { endpointId: 'capture', state, identity: null });
    }
    await assert.rejects(peer.client.observeDesktopCapture(desktop, '')); assert.equal(peer.requests.length, 4);
  } finally { peer.close(); }
});

test('Phone Desktop capture refuses endpoint, missing match, stale identity and malformed incarnation replies', async () => {
  const desktop = { pid: 123, startTimeUtcTicks: '1', imagePath: 'C:\\fixture.exe', appUserModelId: 'Fixture.Package!UI' };
  for (const invalid of ['endpoint', 'missing', 'unavailable', 'pid', 'ticks'] as const) {
    const peer = fixture({ beforeSend: async () => assert.fail('read-only'), beforeResolve: async () => assert.fail('read-only') });
    try {
      const pending = peer.client.observeDesktopCapture(desktop, 'capture'), rejected = assert.rejects(pending); await tick();
      peer.reply(peer.requests[0]!, { endpointId: invalid === 'endpoint' ? 'other' : 'capture', state: invalid === 'unavailable' ? 'unavailable' : 'matched',
        identity: invalid === 'missing' ? null : { instanceId: 'session', pid: invalid === 'pid' ? 0 : 456, processStartTimeUtcTicks: invalid === 'ticks' ? '0123' : '123' } });
      await rejected;
    } finally { peer.close(); }
  }
});

test('Phone persists original intent before dispatch and native receipt before resolving', async () => {
  const before = deferred(), receipt = deferred(); const events: string[] = [];
  const peer = fixture({ beforeSend: async intent => { assert.match(intent.requestHash, /^sha256:[a-f0-9]{64}$/); events.push('intent'); await before.promise; },
    beforeResolve: async () => { events.push('receipt'); await receipt.promise; } });
  try {
    let resolved = false;
    const request = peer.client.request('call.prepare', { callId: randomUUID() }, randomUUID()).then(value => { resolved = true; return value; });
    await tick(); assert.deepEqual(events, ['intent']); assert.equal(peer.requests.length, 0);
    before.resolve(); await tick(); assert.equal(peer.requests.length, 1);
    peer.reply(peer.requests[0]!); await tick(); assert.deepEqual(events, ['intent', 'receipt']); assert.equal(resolved, false);
    receipt.resolve(); assert.equal((await request).ok, true);
  } finally { before.resolve(); receipt.resolve(); peer.close(); }
});
test('Phone reserves bounded IPC capacity for the complete cleanup sequence while ordinary requests are saturated', async () => {
  let intents = 0, receipts = 0;
  const peer = fixture({ beforeSend: async () => { intents++; }, beforeResolve: async () => { receipts++; } });
  const pending: Promise<PhoneReply>[] = [], settled: Promise<PromiseSettledResult<PhoneReply>[]>[] = [];
  const keep = (promise: Promise<PhoneReply>) => { pending.push(promise); settled.push(Promise.allSettled([promise])); };
  try {
    for (let index = 0; index < 32; index++) keep(peer.client.request('desktop.observe', { appUserModelId: 'Fixture.Package!UI' }, null));
    assert.equal(peer.requests.length, 32);
    await assert.rejects(peer.client.request('call.prepare', { callId: randomUUID() }, randomUUID()), { code: 'phone_capacity' });
    assert.equal(intents, 0);
    for (let index = 0; index < 13; index++) keep(peer.client.request('status', {}, null));
    const callId = randomUUID(), desktop = { pid: 123, startTimeUtcTicks: '1', imagePath: 'C:\\fixture.exe', appUserModelId: 'Fixture.Package!UI' };
    const voiceInput = { micro: { usbipExecutable: 'C:\\verified\\usbip.exe', executableHash: 'sha256:' + '0'.repeat(64), port: 3241 }, hotkeyFallback: { modifiers: ['control', 'shift'], keyCode: 32, focusBeforeHotkey: true } };
    for (const [method, params] of [
      ['call.hangup', { callId }], ['call.desktop.stopVoice', { callId, desktop, voiceInput }], ['call.release', { callId }],
    ] as const) {
      validatePhone('ControlMethod', method);
      keep(peer.client.request(method, params, randomUUID())); await tick();
    }
    assert.equal(peer.requests.length, 48); assert.equal(intents, 3);
    await assert.rejects(peer.client.request('heartbeat', {}, null), { code: 'phone_capacity' });
    assert.equal(peer.requests.length, 48);
    for (const request of peer.requests) peer.reply(request);
    assert.ok((await Promise.all(pending)).every(reply => reply.ok)); assert.equal(receipts, 3);
    const next = peer.client.request('heartbeat', {}, null); peer.reply(peer.requests.at(-1)!); assert.equal((await next).ok, true);
  } finally { peer.close(); await Promise.all(settled); }
});

test('Phone pipe loss after dispatch is unknown and does not resend the command', async () => {
  const peer = fixture({ beforeSend: async () => undefined, beforeResolve: async () => undefined });
  try {
    const request = peer.client.request('call.prepare', { callId: randomUUID() }, randomUUID());
    const rejected = assert.rejects(request, { code: 'phone_pipe_closed', outcome: 'unknown' });
    await tick(); assert.equal(peer.requests.length, 1); peer.output.end(); await rejected;
    await assert.rejects(peer.client.request('status', {}, null), { code: 'phone_unavailable', outcome: 'not_executed' });
    assert.equal(peer.requests.length, 1);
  } finally { peer.close(); }
});
test('Phone response epoch, duplicate receipts and oversized frames fail closed', async () => {
  for (const failure of ['epoch', 'duplicate', 'oversize'] as const) {
    const receipt = deferred(); let saves = 0;
    const peer = fixture({ beforeSend: async () => undefined, beforeResolve: async () => { saves++; await receipt.promise; } });
    try {
      const pending = peer.client.request('call.prepare', { callId: randomUUID() }, randomUUID());
      const rejected = assert.rejects(pending, { code: 'phone_invalid_frame', outcome: 'unknown' });
      await tick();
      if (failure === 'oversize') peer.output.write(Buffer.alloc(phoneFrameBytes + 1, 32));
      else if (failure === 'epoch') peer.output.write(JSON.stringify({ version: 1, epoch: randomUUID(), requestId: 1, ok: true, result: {}, error: null }) + '\n');
      else { peer.reply(peer.requests[0]!); peer.reply(peer.requests[0]!); }
      await rejected; assert.equal(saves, failure === 'duplicate' ? 1 : 0);
    } finally { receipt.resolve(); peer.close(); }
  }
});
test('Phone effect deadline retains the owner and journals only its late original reply', async () => {
  let receipts = 0;
  const peer = fixture({ beforeSend: async () => undefined, beforeResolve: async () => { receipts++; } });
  try {
    const pending = peer.client.request('call.prepare', { callId: randomUUID() }, randomUUID(), 20);
    await assert.rejects(pending, { code: 'phone_request_timeout', outcome: 'unknown' });
    const status = peer.client.request('status', {}, null); await tick();
    peer.reply(peer.requests[1]!, { configured: true }); assert.equal((await status).ok, true);
    peer.reply(peer.requests[0]!); await tick();
    assert.equal(peer.requests.length, 2); assert.equal(receipts, 1);
  } finally { peer.close(); }
});
test('Phone reply received before the deadline still settles its original journal after slow storage', async () => {
  const saved = deferred(); let receipts = 0;
  const peer = fixture({ beforeSend: async () => undefined, beforeResolve: async () => { await saved.promise; receipts++; } });
  try {
    const pending = peer.client.request('call.prepare', { callId: randomUUID() }, randomUUID(), 20);
    await tick(); peer.reply(peer.requests[0]!);
    await assert.rejects(pending, { code: 'phone_request_timeout', outcome: 'unknown' });
    saved.resolve(); await tick();
    assert.equal(receipts, 1);
    const status = peer.client.request('status', {}, null); await tick();
    peer.reply(peer.requests[1]!, { configured: true }); assert.equal((await status).ok, true);
  } finally { saved.resolve(); peer.close(); }
});
test('Phone rejects invalid command shapes before durable intent or pipe writes', async () => {
  let intents = 0;
  const peer = fixture({ beforeSend: async () => { intents++; }, beforeResolve: async () => undefined });
  try {
    await assert.rejects(peer.client.request('call.prepare', {}, randomUUID()));
    await assert.rejects(peer.client.request('call.prepare', { callId: 'not-a-call-id' }, randomUUID()));
    await assert.rejects(peer.client.request('call.dial', { callId: randomUUID() }, randomUUID()));
    await assert.rejects(peer.client.request('call.prepare', { callId: randomUUID() }, null));
    await assert.rejects(peer.client.request('status', { unexpected: true }, null));
    assert.equal(intents, 0); assert.equal(peer.requests.length, 0);
  } finally { peer.close(); }
});
test('Phone status remains usable alongside a pending mutation and does not use durable effect hooks', async () => {
  let intents = 0, receipts = 0;
  const peer = fixture({ beforeSend: async () => { intents++; }, beforeResolve: async () => { receipts++; } });
  try {
    const mutation = peer.client.request('call.prepare', { callId: randomUUID() }, randomUUID()); await tick();
    const status = peer.client.request('status', {}, null); await tick();
    peer.reply(peer.requests[1]!, { configured: true }); assert.equal((await status).ok, true);
    assert.equal(intents, 1); assert.equal(receipts, 0);
    peer.reply(peer.requests[0]!); const reply: PhoneReply = await mutation;
    assert.equal(reply.ok, true); assert.equal(receipts, 1);
  } finally { peer.close(); }
});

test('Phone audio preparation validates nested authority and retains the original call intent before dispatch', async () => {
  let intents = 0; const callId = randomUUID(), operationId = randomUUID();
  const peer = fixture({ beforeSend: async intent => {
    intents++; assert.equal(intent.callId, callId); assert.equal(intent.operationId, operationId); assert.equal(intent.method, 'call.audio.prepare');
  }, beforeResolve: async () => undefined });
  const args = { callId, settings: { captureEndpointId: 'fixture-capture', renderEndpointId: 'fixture-render', sourceMode: 'desktop_process',
    captureBufferMs: 20, renderLatencyMs: 10, queueMs: 60 }, desktop: { pid: 123, startTimeUtcTicks: '639241935967464417',
    imagePath: 'C:\\fixture-desktop.exe', appUserModelId: 'Fixture.Package!UI' } };
  try {
    await assert.rejects(peer.client.request('call.audio.prepare', { ...args, desktop: { ...args.desktop, startTimeUtcTicks: 123 } }, operationId));
    await assert.rejects(peer.client.request('call.audio.prepare', { ...args, settings: { ...args.settings, queueMs: 201 } }, operationId));
    await assert.rejects(peer.client.request('call.audio.prepare', { ...args, desktop: { ...args.desktop, voiceConfirmed: true } }, operationId));
    await assert.rejects(peer.client.request('call.audio.prepare', args, null));
    assert.equal(intents, 0); assert.equal(peer.requests.length, 0);
    const pending = peer.client.request('call.audio.prepare', args, operationId); await tick();
    assert.equal(intents, 1); assert.deepEqual(peer.requests[0]!['params'], args);
    const status = { callId, state: 'suspended', route: { state: 'suspended', sourceMode: 'desktop_process', sampleRate: 48000, channels: 1,
      captureDroppedSamples: 0, renderDroppedSamples: 0, captureQueuedSamples: 0, renderQueuedSamples: 0, renderLatencyMs: 10 } };
    validatePhone('AudioAttachmentStatus', status); peer.reply(peer.requests[0]!, status);
    assert.deepEqual((await pending).result, status);
  } finally { peer.close(); }
});

test('Phone capture and process reads preserve original identity and unavailable states without effect hooks or cached authority', async () => {
  let effects = 0; const peer = fixture({ beforeSend: async () => { effects++; }, beforeResolve: async () => { effects++; } });
  const desktop = { pid: 123, startTimeUtcTicks: '1', imagePath: 'C:\\fixture.exe', appUserModelId: 'Fixture.Package!UI' };
  try {
    const capture = peer.client.observeCapture('capture-exact'); await tick();
    assert.deepEqual(peer.requests[0]!['params'], { endpointId: 'capture-exact' }); assert.equal(peer.requests[0]!['operationId'], null);
    const observed = { endpointId: 'capture-exact', state: 'observed', sessions: [
      { instanceId: 'shared', state: 'active', pid: 123, singleProcess: false }, { instanceId: 'single', state: 'inactive', pid: 124, singleProcess: true }] };
    peer.reply(peer.requests[0]!, observed); assert.deepEqual(await capture, observed);
    const process = peer.client.observeAudioProcess(desktop, 123); await tick();
    assert.deepEqual(peer.requests[1]!['params'], { desktop, pid: 123 }); peer.reply(peer.requests[1]!, { pid: 123, state: 'foreign' });
    assert.deepEqual(await process, { pid: 123, state: 'foreign' });
    const unavailable = peer.client.observeCapture('capture-exact'); await tick();
    peer.reply(peer.requests[2]!, { endpointId: 'capture-exact', state: 'unavailable', sessions: [] }); assert.equal((await unavailable).sessions.length, 0);
    await assert.rejects(peer.client.observeCapture('')); await assert.rejects(peer.client.observeAudioProcess(desktop, 0));
    assert.equal(peer.requests.length, 3); assert.equal(effects, 0);
  } finally { peer.close(); }
});

test('Phone capture/process replies cannot change endpoint, PID, session uniqueness or unavailable semantics', async () => {
  const desktop = { pid: 123, startTimeUtcTicks: '1', imagePath: 'C:\\fixture.exe', appUserModelId: 'Fixture.Package!UI' };
  for (const invalid of ['endpoint', 'pid', 'duplicate', 'unavailable', 'single-zero'] as const) {
    const peer = fixture({ beforeSend: async () => assert.fail('read must not retain intent'), beforeResolve: async () => assert.fail('read must not retain receipt') });
    try {
      const request = invalid === 'pid' ? peer.client.observeAudioProcess(desktop, 123) : peer.client.observeCapture('original');
      const failed = assert.rejects(request); await tick();
      const session = { instanceId: 'one', state: 'active', pid: invalid === 'single-zero' ? 0 : 123, singleProcess: true };
      peer.reply(peer.requests[0]!, invalid === 'pid' ? { pid: 124, state: 'owned' } : {
        endpointId: invalid === 'endpoint' ? 'another' : 'original', state: invalid === 'unavailable' ? 'unavailable' : 'observed',
        sessions: invalid === 'duplicate' ? [session, session] : [session] }); await failed;
    } finally { peer.close(); }
  }
});

test('a delayed read and its eventual reply do not terminate another original command', async () => {
  const peer = fixture({ beforeSend: async () => {}, beforeResolve: async () => {} });
  try {
    const call = peer.client.request('call.prepare', { callId: randomUUID() }, randomUUID()); await tick();
    await assert.rejects(peer.client.request('status', {}, null, 20), { code: 'phone_request_timeout' });
    peer.reply(peer.requests[1]!); await tick();
    peer.reply(peer.requests[0]!); assert.equal((await call).ok, true);
    const status = peer.client.request('status', {}, null); peer.reply(peer.requests[2]!);
    assert.equal((await status).ok, true);
  } finally { peer.close(); }
});
