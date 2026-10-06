import { hashJson } from '../../../packages/sdk/src/node.js';
import { need, same } from './schema.js';
import type { Pin } from './schema.js';
import type { MediaAdmission, MediaReadPlan, MediaCoverage, MediaManifest } from './media-schema.js';
import { validateMedia } from './media-schema.js';
import type { OutlookSource } from './outlook-source.js';
import type { MediaSpool } from './media-spool.js';
import { microsoftMcp, outlookHasMedia } from './outlook-projection.js';
import { OutlookMediaNative } from './outlook-media-native.js';
import { prepareAcquiredMediaInput } from './media-document.js';
import type { AcquiredMediaPreparer } from './media-document.js';
import type { PreparedMediaInput } from './media-input.js';

export interface OutlookMediaContext {
  item: Pin;
  coverage: { pin: Pin; value: MediaCoverage };
  readAttachment(index: number): Promise<{ pin: Pin; value: MediaManifest; bytes: Buffer } | null>;
  input(index: number, maximumInputBytes: number): Promise<PreparedMediaInput | null>;
}

/** Fair media acquisition follows saved incoming captures; text collection and triage continue independently. */
export class OutlookMediaSource {
  readonly native: OutlookMediaNative;
  readonly configuration;
  private pending: MediaReadPlan | null = null;
  private notBefore = 0;
  issue: string | null = null;
  constructor(readonly source: OutlookSource, readonly spool: MediaSpool, request: typeof fetch = fetch, readonly prepareInput: AcquiredMediaPreparer = prepareAcquiredMediaInput) {
    this.configuration = source.configuration; this.native = new OutlookMediaNative(source, spool, request);
  }
  /** Read complete original coverage for semantic context, without native calls or reacquisition. */
  async context(item: Pin): Promise<OutlookMediaContext | null> {
    await this.source.engine.verifyOwner();
    const admitted = await this.source.engine.item(item.objectId), link = admitted.value.media.find(value => value.contractKey === 'secretary/media-coverage' && value.pin);
    if (!link?.pin) return null;
    const rawCoverage = await this.source.engine.client.request('objects.read', link.pin);
    need(rawCoverage.object.ownerObjectId === item.objectId && rawCoverage.content.encoding === 'json' && rawCoverage.revision.contentHash === link.contentHash,
      'media_context_mismatch', 'Media context must belong to this item.');
    if (rawCoverage.content.encoding !== 'json') throw Error('Unreachable');
    validateMedia('MediaCoverage', rawCoverage.content.value); const value = rawCoverage.content.value as unknown as MediaCoverage;
    need(value.admission.sourceId === this.configuration.sourceId && value.admission.itemObjectId === item.objectId, 'media_context_mismatch', 'Media context belongs to another source.');
    const saved = { pin: link.pin, value };
    const original = structuredClone(saved);
    const readAttachment: OutlookMediaContext['readAttachment'] = async index => {
      need(Number.isInteger(index) && index >= 0 && index < original.value.attachments.length,
        'media_context_mismatch', 'Select one attachment from the original complete coverage.');
      await this.source.engine.verifyOwner();
      const current = await this.native.publication.coverage(item);
      need(current && same(current, original), 'media_context_mismatch', 'The original media coverage changed.');
      const attachment = original.value.attachments[index]!; if (!attachment.manifest) return null;
      const raw = await this.source.engine.client.request('objects.read', attachment.manifest);
      need(raw.content.encoding === 'json', 'media_context_mismatch', 'Media context requires its original manifest.');
      if (raw.content.encoding !== 'json') throw Error('Unreachable');
      validateMedia('MediaManifest', raw.content.value);
      const value = raw.content.value as unknown as MediaManifest;
      const manifest = await this.native.publication.manifest(attachment.manifest, original.value.admission);
      need(manifest.admission.itemObjectId === item.objectId && same(manifest.evidence.attachment, attachment.attachment),
        'media_context_mismatch', 'The attachment differs from its selected original coverage.');
      const bytes = await this.native.publication.payload(attachment.manifest, original.value.admission);
      return { pin: structuredClone(attachment.manifest), value: manifest, bytes };
    };
    return { item: structuredClone(admitted.pin), coverage: structuredClone(original), readAttachment, input: async (index, maximumInputBytes) => {
      const payload = await readAttachment(index); if (!payload) return null;
      const { evidence } = payload.value;
      return this.prepareInput({ name: evidence.attachment.name, mediaType: evidence.attachment.mediaType,
        byteLength: evidence.byteLength, contentHash: evidence.contentHash }, payload.bytes, maximumInputBytes);
    } };
  }
  private async select(): Promise<MediaReadPlan | null> {
    const engine = this.source.engine, publications = engine.store.technicalList<import('./microsoft-schema.js').OutlookPublication>('secretary/outlook-publication').slice(0, 8);
    for (const entry of publications) {
      const publication = await this.source.store.read('secretary/outlook-publication', entry.pin);
      const raw = await this.source.store.read('secretary/outlook-evidence', publication.value.evidence);
      const originalPlan = await this.source.store.read('secretary/outlook-plan', raw.value.plan);
      if (originalPlan.value.configuration.sourceId !== this.configuration.sourceId || !publication.value.captured.length) continue;
      const proof = await this.source.evidence(publication.value.evidence);
      if (proof.value.gap || !proof.value.page?.messagesWithAttachments) continue;
      const calls = await Promise.all(proof.value.calls.map(pin => this.source.store.read('secretary/outlook-call', pin))), list = calls.find(value => value.value.slot === 3);
      need(list?.value.observation, 'media_source_evidence_missing', 'Media selection needs the retained original message page.');
      const operation = await this.source.store.operation(proof.value.plan, 3, list.value.observation);
      need(operation.reply && 'result' in operation.reply, 'media_source_evidence_missing', 'Media selection needs the original successful list result.');
      const entries = microsoftMcp(operation.reply.result)['value'] as Record<string, unknown>[];
      for (const pin of publication.value.captured) {
        const capture = await engine.store.read('secretary/operation', pin);
        need(capture.value.phase === 'succeeded' && capture.value.action === 'capture' && capture.value.effect, 'media_source_evidence_missing', 'Only a successfully admitted original capture can select media.');
        const item = await engine.item(capture.value.effect), message = item.value.message;
        const original = entries.find(value => typeof value['id'] === 'string' && hashJson(['outlook-message', value['id']]) === message.messageId);
        if (!original || !outlookHasMedia(original)) continue;
        const old = this.spool.readPlan(item.pin);
        const covered = await this.native.publication.coverage(item.pin);
        if (covered) {
          if (old) { await this.native.verifyCoverage(old, covered.value); this.spool.archivePublished(old); }
          this.issue ??= covered.value.gap ?? covered.value.attachments.find(value => value.errorCode)?.errorCode ?? null; continue;
        }
        const admission: MediaAdmission = { sourceId: this.configuration.sourceId, producerPrincipalId: engine.settings.identity.principalId,
          accountId: message.accountId, providerMessageId: String(original['id']), itemObjectId: item.pin.objectId,
          identityHash: item.value.identityHash, messageHash: item.value.messageHash };
        return this.native.prepare(admission, item.pin, capture.value.operationId);
      }
    }
    this.notBefore = Date.now() + this.configuration.pollMs; return null;
  }
  async step(): Promise<'idle' | 'pending' | 'complete'> {
    if (!this.pending && Date.now() < this.notBefore) return 'idle';
    await this.source.engine.verifyOwner(); this.pending ??= await this.select(); if (!this.pending) return 'idle';
    const result = await this.native.step(this.pending); if (result.phase === 'pending') return 'pending';
    const saved = await this.native.publication.saveCoverage(result.coverage, this.pending);
    await this.native.verifyCoverage(this.pending, saved.value);
    this.spool.archivePublished(this.pending);
    this.issue ??= saved.value.gap ?? saved.value.attachments.find(value => value.errorCode)?.errorCode ?? null;
    this.pending = null; return 'complete';
  }
}
