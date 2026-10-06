import { canonical, hashJson } from '../../../packages/sdk/src/node.js';
import type { Wire } from '../../../packages/sdk/src/node.js';
import { same } from './schema.js';
import type { Pin } from './schema.js';
import type { OutlookMediaContext } from './outlook-media-source.js';
import type { PreparedMediaInput } from './media-input.js';
import type { TriageMedia, TriageMediaAttachment } from './triage-schema.js';
import { triageCheck as check } from './triage-store.js';

export interface TriageMediaContext {
  proof: { pin: Pin; contractKey: TriageMedia['coverageContract']; contentHash: string };
  item: Pin; sourceId: string; gap: string | null;
  attachments: { name: string | null; mediaType: string | null; manifest: Pin | null; errorCode: string | null; representation?: TriageMediaAttachment['representation'] }[];
  input(index: number, maximumInputBytes: number): Promise<PreparedMediaInput | null>;
}
export type TriageMediaResolver = (sourceId: string, item: Pin) => Promise<TriageMediaContext | null>;
export function outlookTriageMedia(context: OutlookMediaContext): TriageMediaContext {
  const { pin, value } = context.coverage;
  return { proof: { pin: structuredClone(pin), contractKey: 'secretary/media-coverage', contentHash: hashJson(value) },
    item: structuredClone(context.item), sourceId: value.admission.sourceId, gap: value.gap,
    attachments: value.attachments.map(entry => ({ name: entry.attachment.name, mediaType: entry.attachment.mediaType,
      manifest: structuredClone(entry.manifest), errorCode: entry.errorCode })), input: (index, budget) => context.input(index, budget) };
}
export const triageRequestFrameBytes = 6 * 1024 * 1024;
// The complete prompt, output schema, configured restrictions and native envelope retain their own budget.
const mediaInputBytes = triageRequestFrameBytes - 512 * 1024;

/** Only an internally verified original coverage reader can supply semantic attachment input. */
export async function triageMedia(context: TriageMediaContext, item: Pin, sourceId: string): Promise<{ media: TriageMedia; inputs: Record<string, Wire.Json>[] }> {
  const original = structuredClone({ proof: context.proof, item: context.item, sourceId: context.sourceId, gap: context.gap, attachments: context.attachments });
  check(same(original.item, item) && original.sourceId === sourceId, 'secretary_triage_media_mismatch');
  const media: TriageMedia = { coverage: original.proof.pin, coverageContract: original.proof.contractKey, coverageHash: original.proof.contentHash, gap: original.gap ??
    (original.attachments.length === 0 ? 'media_expected_but_empty' : null), attachments: [] };
  const inputs: Record<string, Wire.Json>[] = []; let remaining = mediaInputBytes;
  for (let index = 0; index < original.attachments.length; index++) {
    const attachment = original.attachments[index]!, source = attachment;
    const entry: TriageMediaAttachment = { name: source.name, mediaType: source.mediaType, manifest: attachment.manifest,
      byteLength: null, contentHash: null, representation: source.representation ?? 'original_bytes', kind: 'unavailable', inputIndex: null, errorCode: attachment.errorCode };
    if (!attachment.manifest) { check(attachment.errorCode, 'secretary_triage_media_mismatch'); media.attachments.push(entry); continue; }
    const label = { type: 'text', text: canonical({ attachment: index + 1, name: source.name, mediaType: source.mediaType, representation: entry.representation }), text_elements: [] };
    const labelBytes = Buffer.byteLength(canonical(label)) + 2;
    // Even an exhausted frame still verifies the source and retains an explicit gap, never a partial body.
    const prepared = await context.input(index, Math.max(1, remaining - labelBytes));
    check(prepared && prepared.source.name === source.name && prepared.source.mediaType === source.mediaType, 'secretary_triage_media_mismatch');
    entry.byteLength = prepared!.source.byteLength; entry.contentHash = prepared!.source.contentHash;
    if (prepared!.kind === 'unavailable') entry.errorCode = prepared!.code;
    else {
      const parts = prepared!.kind === 'video' ? prepared!.inputs : [prepared!.input];
      const used = labelBytes + Buffer.byteLength(canonical(parts));
      if (used > remaining) entry.errorCode = 'media_native_input_too_large';
      else {
        entry.kind = prepared!.kind; entry.inputIndex = inputs.length + 2; entry.errorCode = null;
        inputs.push(label, ...parts); remaining -= used;
      }
    }
    media.attachments.push(entry);
  }
  return { media, inputs };
}
