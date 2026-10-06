import assert from 'node:assert/strict';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { parseArgs } from 'node:util';
import { createHash } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { chromium } from '@playwright/test';
import { HiveClient, discover, callBound } from '../../../dist/packages/sdk/src/client.js';
import { atomicJson } from '../../../dist/packages/host-runtime/src/config.js';
import { nativeMediaAdapter } from '../support/native-media-adapter.mjs';

// Actual native transport preflight. A new headless peer has no microphone, camera or user
// browser profile. SDP goes through the exact installed AgentManager projection. No SIP call.
const { values } = parseArgs({ options: { config: { type: 'string' }, evidence: { type: 'string' }, id: { type: 'string' }, 'expected-host': { type: 'string' }, protocol: { type: 'string', default: 'v3' }, 'media-adapter': { type: 'boolean' }, 'desktop-media-route': { type: 'boolean' } } });
for (const key of ['config', 'evidence', 'id', 'expected-host']) assert.ok(values[key], 'Missing --' + key);
assert.match(values.id, /^[a-z0-9-]{1,80}$/);
assert.ok(['v1', 'v2', 'v3'].includes(values.protocol));
assert.ok(!(values['media-adapter'] && values['desktop-media-route']));
const root = resolve(values.evidence), config = JSON.parse(await readFile(resolve(values.config), 'utf8'));
assert.equal(config.hostId, values['expected-host']); assert.ok(config.hostId.endsWith('-acceptance'));
const agent = config.instances.find(value => value.componentId === 'agent-manager'); assert.ok(agent?.credential);
const client = new HiveClient(config.publicBaseUrl, { credential: agent.credential }), node = agent.serviceNodeId, id = values.id;
await mkdir(root, { recursive: true });
await writeFile(join(root, 'invocation.json'), JSON.stringify({ id, node, hostId: config.hostId }), { flag: 'wx', mode: 0o600 });
await mkdir(join(root, 'project'), { recursive: true });
const sha = value => createHash('sha256').update(value).digest('hex');
const report = { schemaVersion: 1, id, protocol: values.protocol, hostId: config.hostId, node, startedAt: new Date().toISOString(), phase: 'starting', threadId: null, native: null, authType: null, actions: [], events: [], media: null, failure: null, cleanup: {} };
const save = () => atomicJson(join(root, 'report.json'), report);
const call = async (name, args = {}, operation) => {
  const binding = await discover(client, name, { serviceNodeId: node });
  if (operation) { const recorded = structuredClone(args); if (recorded.transport?.sdp) recorded.transport.sdp = { sha256: sha(recorded.transport.sdp), bytes: Buffer.byteLength(recorded.transport.sdp) };
    report.actions.push({ name, arguments: recorded, operationId: operation, definitionHash: binding.definitionHash }); await save(); }
  return callBound(client, binding, args, operation, { timeoutMs: 30000 });
};
let sequence = 0, startDispatched = false, browser, page, adapter;
async function events() {
  const batch = await call('agent.notifications', { afterSequence: sequence, limit: 100 });
  assert.equal(batch.epoch, report.native.epoch); assert.equal(batch.gap, false);
  let sdp;
  for (const event of batch.items.filter(value => value.params?.threadId === report.threadId && value.method.startsWith('thread/realtime/'))) {
    const recorded = structuredClone(event);
    if (recorded.method === 'thread/realtime/sdp') { sdp = recorded.params.sdp; recorded.params.sdp = { sha256: sha(sdp), bytes: Buffer.byteLength(sdp) }; }
    report.events.push(recorded);
  }
  sequence = batch.throughSequence; await save(); return sdp;
}
const errorObserved = () => report.events.find(value => value.method === 'thread/realtime/error');
try {
  const status = await call('agent.status'); assert.equal(status.state, 'ready'); assert.equal(status.nativeVersion, '0.154.0'); assert.equal(status.hostId, config.hostId);
  report.native = Object.fromEntries(['epoch', 'nativeVersion', 'nativeExecutableHash', 'catalogHash', 'serviceNodeId', 'hostId'].map(key => [key, status[key]]));
  const account = await call('codex.account/read', { refreshToken: false }, id + '-account'); report.authType = account.account?.type ?? null;
  const head = await call('agent.notifications', { afterSequence: Number.MAX_SAFE_INTEGER, limit: 1 }); sequence = head.throughSequence;
  browser = await chromium.launch({ headless: true }); page = await browser.newPage();
  const offer = await page.evaluate(async () => {
    const pc = new RTCPeerConnection({ iceServers: [] }); window.ivyPeer = pc; window.ivyEvents = [];
    pc.addTransceiver('audio', { direction: 'recvonly' });
    const dc = pc.createDataChannel('oai-events'); window.ivyChannel = dc;
    dc.onmessage = event => { if (window.ivyEvents.length < 100) { const value = JSON.parse(event.data); window.ivyEvents.push({ type: value.type, error: value.error ?? null }); } };
    await pc.setLocalDescription(await pc.createOffer()); return pc.localDescription.sdp;
  });
  if (values['media-adapter']) { assert.equal(values.protocol, 'v3'); adapter = await nativeMediaAdapter(root); }
  const callBase = adapter?.baseUrl ?? (values['desktop-media-route'] ? 'https://chatgpt.com/backend-api/wham' : null);
  const thread = await call('codex.thread/start', { cwd: join(root, 'project'), ephemeral: true, permissions: ':danger-full-access', approvalPolicy: 'never', config: { 'features.realtime_conversation': true, ...(callBase ? { experimental_realtime_webrtc_call_base_url: callBase } : {}) } }, id + '-create');
  report.threadId = thread.thread.id; assert.equal(thread.thread.ephemeral, true); await save();
  startDispatched = true;
  await call('codex.thread/realtime/start', { threadId: report.threadId, outputModality: 'audio', version: values.protocol, voice: values.protocol === 'v2' ? 'marin' : 'cove', transport: { type: 'webrtc', sdp: offer }, includeStartupContext: false, clientManagedHandoffs: true,
    prompt: 'This is an isolated voice transport check. Say only the brief test text when requested. Do not invoke tools.' }, id + '-start');
  report.phase = 'waiting-for-sdp'; await save(); let answer;
  const end = Date.now() + 30000;
  while (Date.now() < end && !answer) { answer = await events(); if (errorObserved()) throw Error(errorObserved().params.message); if (!answer) await delay(250); }
  assert.ok(answer, 'No original native SDP answer arrived.');
  await page.evaluate(sdp => window.ivyPeer.setRemoteDescription({ type: 'answer', sdp }), answer);
  await page.waitForFunction(() => window.ivyPeer.connectionState === 'connected' && window.ivyChannel.readyState === 'open', undefined, { timeout: 30000 });
  await call('codex.thread/realtime/appendText', { threadId: report.threadId, text: 'Say: Isolated Ivy voice transport check.' }, id + '-text');
  const audioEnd = Date.now() + 30000;
  while (Date.now() < audioEnd) {
    report.media = await page.evaluate(async () => ({ connection: window.ivyPeer.connectionState, dataChannel: window.ivyChannel.readyState, events: window.ivyEvents,
      inbound: [...(await window.ivyPeer.getStats()).values()].filter(row => row.type === 'inbound-rtp' && row.kind === 'audio').map(row => ({ bytesReceived: row.bytesReceived, packetsReceived: row.packetsReceived, totalAudioEnergy: row.totalAudioEnergy, totalSamplesReceived: row.totalSamplesReceived })) }));
    if (report.media.inbound.some(row => row.bytesReceived > 0 && row.totalAudioEnergy > 0)) break;
    await events(); if (errorObserved()) throw Error(errorObserved().params.message); await delay(250);
  }
  assert.ok(report.media?.inbound.some(row => row.bytesReceived > 0 && row.totalAudioEnergy > 0), 'No non-silent received native audio.'); report.phase = 'native-audio-received';
} catch (error) { report.phase = 'failed'; report.failure = { code: error.code ?? error.name, message: error.message, details: error.details ?? null }; process.exitCode = 1; }
finally {
  if (report.threadId) try {
    assert.equal((await call('agent.status')).epoch, report.native.epoch);
    if (startDispatched) { await call('codex.thread/realtime/stop', { threadId: report.threadId }, id + '-stop'); report.cleanup.stopReceipt = true; await events(); }
    report.cleanup.unsubscribe = await call('codex.thread/unsubscribe', { threadId: report.threadId }, id + '-unsubscribe');
  } catch (error) { report.cleanup.failure = { code: error.code ?? error.name, message: error.message }; process.exitCode = 1; }
  if (page) await page.evaluate(() => window.ivyPeer?.close()).catch(() => undefined);
  if (browser) await browser.close();
  if (adapter) { report.adapter = adapter.summary(); await adapter.close(); }
  report.finishedAt = new Date().toISOString(); await save(); process.stdout.write(JSON.stringify({ phase: report.phase, root, threadId: report.threadId, failure: report.failure, cleanup: report.cleanup }) + '\n');
}
