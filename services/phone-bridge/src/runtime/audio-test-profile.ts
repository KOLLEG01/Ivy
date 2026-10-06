import { requireThat } from '../../../../packages/sdk/src/node.js';
import { validatePhone } from './native.js';

const preferences = {
  G722Only: ['G722'], EvsOnly: ['EVS'],
  EvsPreferred: ['EVS', 'G722', 'PCMA', 'PCMU'],
  G722Preferred: ['G722', 'PCMA', 'PCMU'],
};

/** Transform only explicit audio test fields; callers retain the surrounding Hive configuration. */
export function audioTestProfile<T extends { codecs: Record<string, unknown>; audio: Record<string, unknown>; windowsAudio?: Record<string, unknown> }>(
  settings: T, codecProfile: string, latencyPreset: string): T {
  requireThat(Object.hasOwn(preferences, codecProfile) && ['Optimized', 'Conservative'].includes(latencyPreset),
    'invalid_arguments', 'An exact IvySIP codec profile and Optimized or Conservative latency preset are required.');
  const result = structuredClone(settings), optimized = latencyPreset === 'Optimized';
  result.codecs = { ...result.codecs, preferences: [...preferences[codecProfile as keyof typeof preferences]], packetMs: 20 };
  validatePhone('CodecSettings', result.codecs);
  for (const field of ['audio', 'windowsAudio'] as const) {
    const audio = result[field];
    if (!audio) continue;
    Object.assign(audio, { captureBufferMs: optimized ? 20 : 40, queueMs: optimized ? 80 : 120,
      playbackPrebufferMs: optimized ? 20 : 40, renderLatencyMs: optimized ? 20 : 60, enableWasapiLowLatency: optimized });
    validatePhone('AudioSettings', audio);
  }
  return result;
}
