import test from 'node:test';
import assert from 'node:assert/strict';
import { audioSetup } from '../services/phone-bridge/src/runtime/audio-setup.js';
import type { PhoneAudioInventory } from '../services/phone-bridge/src/runtime/audio-setup.js';
import { validatePhoneService } from '../services/phone-bridge/src/runtime/registry.js';
import { validatePhone } from '../services/phone-bridge/src/runtime/native.js';

test('Voice and Windows audio contracts accept bounded optional playback prebuffer', () => {
  const settings = { captureEndpointId: 'capture', renderEndpointId: 'render', sourceMode: 'desktop_process',
    captureBufferMs: 20, renderLatencyMs: 20, queueMs: 80 };
  validatePhone('AudioSettings', settings);
  for (const enableWasapiLowLatency of [true, false]) validatePhone('AudioSettings', { ...settings, enableWasapiLowLatency });
  assert.throws(() => validatePhone('AudioSettings', { ...settings, enableWasapiLowLatency: 'true' }));
  for (const playbackPrebufferMs of [0, 20, 40, 200]) validatePhone('AudioSettings', { ...settings, queueMs: 200, playbackPrebufferMs });
  for (const playbackPrebufferMs of [-1, 201, 1.5, '20']) assert.throws(() => validatePhone('AudioSettings', { ...settings, playbackPrebufferMs }));
});

test('audio diagnostics retain bounded actual low-latency fallback observations', () => {
  const status = { state: 'open', sourceMode: 'desktop_process', sampleRate: 48000, channels: 1,
    captureDroppedSamples: 0, renderDroppedSamples: 0, captureQueuedSamples: 0, renderQueuedSamples: 0, renderLatencyMs: 20 };
  validatePhone('MediaStatus', status);
  validatePhone('MediaStatus', { ...status, lowLatencyActive: true, lowLatencyUnavailableReason: null });
  validatePhone('MediaStatus', { ...status, lowLatencyActive: false, lowLatencyUnavailableReason: 'Device does not support IAudioClient3' });
  assert.throws(() => validatePhone('MediaStatus', { ...status, lowLatencyUnavailableReason: 'x'.repeat(1025) }));
});

const profiles = ['voice', 'windows'].map(route => ({ route, settings: {
  captureEndpointId: 'capture', renderEndpointId: 'receive', sourceMode: 'desktop_process',
  mutePolicy: { mutedRenderEndpointId: 'speaker', unmuteReceive: true },
} }));
const inventory: PhoneAudioInventory = { processLoopbackSupported: true, devices: [
  { id: 'capture', name: 'Capture', direction: 'capture', state: 'Active', muted: false },
  { id: 'receive', name: 'Receive', direction: 'render', state: 'Active', muted: false },
  { id: 'speaker', name: 'Speaker', direction: 'render', state: 'Active', muted: true },
] };
test('microphone and virtual speaker use their actual device directions without process loopback', () => {
  for (const sourceMode of ['microphone', 'virtual_speaker']) {
    const settings = { captureEndpointId: 'capture', renderEndpointId: 'receive', loopbackRenderEndpointId: 'speaker',
      sourceMode, captureBufferMs: 20, renderLatencyMs: 20, queueMs: 80 };
    validatePhone('AudioSettings', settings);
    const report = audioSetup({ ...inventory, processLoopbackSupported: false }, [{ route: 'windows', settings }, profiles[0]!]);
    validatePhoneService('PhoneAudioSetup', report);
    assert.equal(report.routes[0]!.ready, true);
    assert.deepEqual(report.routes[0]!.endpoints.map(endpoint => endpoint.role), [sourceMode === 'microphone' ? 'capture' : 'loopback', 'receive']);
    if (sourceMode === 'virtual_speaker') assert.equal(audioSetup(inventory, [{ route: 'windows', settings: {
      ...settings, loopbackRenderEndpointId: 'receive',
    } }]).routes[0]!.ready, false);
  }
});
test('audio setup reports exact endpoint readiness without changing configuration or observations', () => {
  const before = structuredClone(inventory), report = audioSetup(inventory, profiles);
  assert.ok(report.routes.every(route => route.ready)); validatePhoneService('PhoneAudioSetup', report);
  assert.deepEqual(inventory, before);
  for (const [change, state] of [
    [{ muted: false }, 'mute_mismatch'], [{ muted: null }, 'mute_unknown'], [{ state: 'Disabled' }, 'inactive'],
    [{ direction: 'capture' as const }, 'wrong_direction'], [{ id: 'foreign' }, 'missing'],
  ] as const) {
    const changed = structuredClone(inventory); Object.assign(changed.devices[2]!, change);
    const result = audioSetup(changed, profiles); validatePhoneService('PhoneAudioSetup', result);
    assert.equal(result.routes[0]!.ready, false); assert.equal(result.routes[0]!.endpoints[2]!.state, state);
  }
  const duplicate = structuredClone(inventory); duplicate.devices.push(duplicate.devices[2]!);
  assert.equal(audioSetup(duplicate, profiles).routes[0]!.endpoints[2]!.state, 'ambiguous');
  assert.ok(audioSetup({ ...inventory, processLoopbackSupported: false }, profiles).routes.every(route => !route.ready));
  const conflicting = profiles.map(profile => ({ ...profile, settings: { ...profile.settings,
    mutePolicy: { mutedRenderEndpointId: 'receive', unmuteReceive: false } } }));
  const mutedReceive = structuredClone(inventory); mutedReceive.devices[1]!.muted = true;
  assert.ok(audioSetup(mutedReceive, conflicting).routes.every(route => !route.ready && route.mutePolicyConflict));
});
