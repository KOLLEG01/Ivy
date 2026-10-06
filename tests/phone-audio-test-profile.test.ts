import test from 'node:test';
import assert from 'node:assert/strict';
import { audioTestProfile } from '../services/phone-bridge/src/runtime/audio-test-profile.js';

const expected = { G722Only: ['G722'], EvsOnly: ['EVS'], EvsPreferred: ['EVS', 'G722', 'PCMA', 'PCMU'], G722Preferred: ['G722', 'PCMA', 'PCMU'] };
const settings = {
  codecs: { preferences: ['OPUS'], packetMs: 20, receiveReorderMs: 40 },
  audio: { captureEndpointId: 'capture', renderEndpointId: 'render', sourceMode: 'desktop_process', captureBufferMs: 30,
    renderLatencyMs: 30, queueMs: 100, mutePolicy: { unmuteReceive: true } },
  windowsAudio: { captureEndpointId: 'windows-capture', renderEndpointId: 'windows-render', sourceMode: 'system_excluding_runtime',
    captureBufferMs: 30, renderLatencyMs: 30, queueMs: 100 },
  credentialsPath: 'untouched-secret-reference', binding: { outgoingOfferMode: 'delayed' }, policy: { untouched: true },
};
for (const [codec, order] of Object.entries(expected)) for (const latency of ['Optimized', 'Conservative']) {
  test(`IvySIP test profile ${codec}/${latency} preserves Hive routing and credentials`, () => {
    const before = structuredClone(settings), result = audioTestProfile(settings, codec, latency);
    assert.deepEqual(settings, before); assert.deepEqual(result.codecs.preferences, order);
    assert.equal(result.codecs.receiveReorderMs, 40); assert.deepEqual(result.binding, settings.binding);
    assert.equal(result.credentialsPath, settings.credentialsPath); assert.deepEqual(result.policy, settings.policy);
    for (const key of ['audio', 'windowsAudio'] as const) {
      const audio = result[key] as Record<string, unknown>, original = settings[key];
      assert.equal(audio.captureEndpointId, original.captureEndpointId); assert.equal(audio.renderEndpointId, original.renderEndpointId);
      assert.equal(audio.sourceMode, original.sourceMode);
      assert.deepEqual([audio.captureBufferMs, audio.queueMs, audio.playbackPrebufferMs, audio.renderLatencyMs, audio.enableWasapiLowLatency],
        latency === 'Optimized' ? [20, 80, 20, 20, true] : [40, 120, 40, 60, false]);
    }
    assert.deepEqual(result.audio.mutePolicy, settings.audio.mutePolicy);
  });
}
test('invalid profiles cannot silently fall back or invent a separate Windows route', () => {
  assert.throws(() => audioTestProfile(settings, 'Unknown', 'Optimized'));
  assert.throws(() => audioTestProfile(settings, 'EvsOnly', 'Unknown'));
  const { windowsAudio, ...single } = settings;
  assert.equal('windowsAudio' in audioTestProfile(single, 'G722Only', 'Optimized'), false);
});
