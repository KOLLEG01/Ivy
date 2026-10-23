import test from 'node:test';
import assert from 'node:assert/strict';
import { validatePhone } from '../tools/operations/phone-contracts.js';

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
