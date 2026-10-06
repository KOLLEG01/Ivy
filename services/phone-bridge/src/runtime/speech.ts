import { randomUUID } from 'node:crypto';
import { mkdir, writeFile } from 'node:fs/promises';
import { isAbsolute, join } from 'node:path';
import { canonical, digest } from '../../../../packages/sdk/src/node.js';
import { IvyError, requireThat } from '../../../../packages/sdk/src/node.js';
import type { PhoneCall } from './admission.js';
import type { PhoneJournal } from './journal.js';
import type { PhoneIntent } from './native.js';
import { phoneConfigurationSection } from './settings.js';

export interface PhoneSpeechSettings { credentialsPath: string; endpoint: string; model: string; voice: string; instructions?: string; speed?: number }
export interface PhoneAnnouncement { wavPath: string; sha256: string }

/** Synthesis is an original effect with a durable receipt. Neither provider errors nor the API
 * credential enter logs. Unknown requests cannot be replayed by requesting the same call again. */
export async function synthesizeAnnouncement(journal: PhoneJournal, call: PhoneCall, text: string, settings: PhoneSpeechSettings,
  fetcher: typeof fetch = fetch): Promise<PhoneAnnouncement> {
  requireThat(settings && isAbsolute(settings.credentialsPath) && text.length > 0 && text.length <= 4096,
    'invalid_arguments', 'Screening needs speech settings and a bounded announcement.');
  const endpoint = new URL(settings.endpoint);
  requireThat(endpoint.protocol === 'https:' && !endpoint.username && !endpoint.password && !endpoint.search && !endpoint.hash,
    'invalid_arguments', 'Speech endpoint requires HTTPS without embedded credentials.');
  const requestHash = digest(canonical({ text, settings }));
  const prior = journal.callCommand(call.callId, 'call.screening.synthesize');
  if (prior) {
    requireThat(prior.intent.requestHash === requestHash, 'mutation_conflict', 'Original speech request changed.');
    requireThat(prior.phase === 'result' && prior.receipt?.ok, 'phone_speech_unknown', 'Original speech synthesis cannot be repeated.');
    return prior.receipt.result as PhoneAnnouncement;
  }
  const intent: PhoneIntent = { epoch: call.epoch, operationId: randomUUID(), callId: call.callId, method: 'call.screening.synthesize', requestHash };
  const credentials = await phoneConfigurationSection(settings.credentialsPath,'speech') as {apiKey:string};
  requireThat(typeof credentials.apiKey === 'string' && credentials.apiKey.length > 0 && credentials.apiKey.length <= 4096 && !/[\r\n]/.test(credentials.apiKey),
    'invalid_arguments', 'Protected speech API credential required.');
  journal.submit(intent);
  try {
    const response = await fetcher(endpoint, { method: 'POST', redirect: 'error', signal: AbortSignal.timeout(60000),
      headers: { authorization: 'Bearer ' + credentials.apiKey, 'content-type': 'application/json' },
      body: JSON.stringify({ model: settings.model, voice: settings.voice, input: text, response_format: 'wav', speed: settings.speed ?? 1,
        ...(settings.instructions ? { instructions: settings.instructions } : {}) }) });
    requireThat(response.body, 'phone_speech_failed', 'Speech provider did not return an announcement.');
    const chunks: Uint8Array[] = []; let bytes = 0;
    const reader = response.body.getReader();
    try {
      requireThat(response.ok, 'phone_speech_failed', 'Speech provider did not return an announcement.');
      while (true) {
        const { value: chunk, done } = await reader.read(); if (done) break;
        bytes += chunk.length;
        requireThat(bytes <= 10 * 1024 * 1024, 'phone_speech_failed', 'Speech announcement exceeds the size bound.'); chunks.push(chunk);
      }
    } finally { await reader.cancel().catch(() => undefined); reader.releaseLock(); }
    const wav = Buffer.concat(chunks);
    requireThat(wav.length >= 44 && wav.subarray(0, 4).toString() === 'RIFF' && wav.subarray(8, 12).toString() === 'WAVE',
      'phone_speech_failed', 'Speech provider did not return WAV.');
    const root = join(journal.dataRoot, 'announcements'); await mkdir(root, { recursive: true, mode: 0o700 });
    const result = { wavPath: join(root, intent.operationId + '.wav'), sha256: digest(wav) };
    await writeFile(result.wavPath, wav, { flag: 'wx', mode: 0o600 });
    journal.finish(intent, { version: 1, epoch: call.epoch, requestId: 1, ok: true, result, error: null });
    return result;
  } catch {
    journal.finish(intent, { version: 1, epoch: call.epoch, requestId: 1, ok: false, result: null, error: 'operation_outcome_unknown' });
    throw new IvyError('phone_speech_unknown', 'Original speech synthesis failed or has an unknown outcome; it was not repeated.', 'unknown');
  }
}
