import { requireThat } from '../../../../packages/sdk/src/node.js';
import type { PhoneSpeakerVoiceCatalog } from './voice-selection.js';

/** Catalog names are capability groups, not permission to change the wire protocol. */
export function selectSpeakerCatalog(
  voices: Record<string, unknown>,
  supportsStart: (protocol: string, voiceId: string) => boolean,
  configuredVoice?: unknown,
): PhoneSpeakerVoiceCatalog {
  const candidates = Object.keys(voices).filter(key => /^v[1-9][0-9]*$/.test(key))
    .sort((a, b) => BigInt(a.slice(1)) > BigInt(b.slice(1)) ? -1 : 1);
  for (const key of candidates) {
    const ids = voices[key];
    requireThat(Array.isArray(ids) && ids.length <= 256 &&
      ids.every(id => typeof id === 'string' && id.length > 0 && id.length <= 256) &&
      new Set(ids).size === ids.length,
    'phone_speaker_catalog_invalid', 'Native speaker catalog must contain unique voice IDs.');
    if (!ids.length) continue;
    // V1 was the speaker group used by the original native V3 implementation.
    // V2 lacks the role-bearing startup and backing-task handoff used by PhoneBridge.
    // Newer groups need explicit support in the pinned native start contract.
    const protocol = key === 'v1' ? 'v3' : key;
    if (key === 'v2' || !ids.every(id => supportsStart(protocol, id))) continue;
    const defaultVoiceId = configuredVoice ?? voices['defaultV' + key.slice(1)] ?? voices['defaultV1'];
    requireThat(typeof defaultVoiceId === 'string' && ids.includes(defaultVoiceId),
      configuredVoice === undefined || configuredVoice === null ? 'phone_speaker_catalog_invalid' : 'phone_speaker_voice_invalid',
      'Default speaker voice is outside the selected native catalog.');
    return { protocol, catalogVersion: key, defaultVoiceId,
      voices: ids.map(id => ({ id, displayName: id.charAt(0).toUpperCase() + id.slice(1) })) };
  }
  requireThat(false, 'phone_speaker_catalog_invalid',
    'No nonempty speaker catalog supports the required native Voice start contract.');
}
