import { canonical, deriveOperationId, digest, hashJson } from '../../../packages/sdk/src/node.js';
import type { Wire } from '../../../packages/sdk/src/node.js';
import { need, same } from './schema.js';
import type { Pin } from './schema.js';
import type { SecretaryEngine } from './engine.js';
import { identityHash, messageHash } from './engine.js';
import type { OutlookSource } from './outlook-source.js';
import { microsoftMcp, outlookHasMedia } from './outlook-projection.js';
import schema from '../../../specs/schemas/secretary-media.schema.json' with { type: 'json' };
import { validateMedia } from './media-schema.js';
import type { MediaAdmission, MediaIntent, MediaManifest, MediaCoverage, MediaReadPlan } from './media-schema.js';
import type { MediaSpool } from './media-spool.js';

const check = (value: unknown) => need(value, 'media_publication_invalid', 'Media publication needs its exact admitted original item and complete immutable bytes.');
const nameFor = (kind: string, intent: MediaIntent, index?: number) => 'media-' + kind + '-' + hashJson([intent, index ?? null]).slice(7);
const chunkBytes = 1048576;
export function mediaRegistry(): Pick<Wire.RegistrySync, 'contracts' | 'requiredContracts'> {
  const contracts: Wire.DataContract[] = [
    { key: 'secretary/media-manifest', version: '1.0.0', owner: { kind: 'service', serviceName: 'secretary' }, mediaType: 'application/json',
      retention: { objects: { mode: 'owned' }, revisions: { mode: 'current' } }, jsonSchema: { ...schema, $ref: '#/$defs/MediaManifest' }, specMarkdown: 'Final verified media manifest owned by its retained Secretary item. Its exact chunk references protect the required bytes.' },
    { key: 'secretary/media-coverage', version: '1.0.0', owner: { kind: 'service', serviceName: 'secretary' }, mediaType: 'application/json',
      retention: { objects: { mode: 'owned' }, revisions: { mode: 'current' } }, jsonSchema: { ...schema, $ref: '#/$defs/MediaCoverage' }, specMarkdown: 'Final useful media coverage summary owned by its Secretary item, with exact references to successful manifests. Acquisition state remains local.' },
    { key: 'secretary/media-chunk', version: '1.0.0', owner: { kind: 'service', serviceName: 'secretary' }, mediaType: 'application/octet-stream',
      retention: { objects: { mode: 'owned' }, revisions: { mode: 'current' } }, specMarkdown: 'One final binary chunk owned by the retained Secretary item and protected by its manifest reference. Never render untrusted attachment content as HTML.' },
  ];
  return { contracts, requiredContracts: contracts.map(value => ({ key: value.key, readVersions: [value.version], writeVersions: [value.version] })) };
}

/** Verify the existing native body page and successful Secretary admission, not a caller-supplied filename. */
export async function verifyOutlookMediaAdmission(source: OutlookSource, admission: MediaAdmission, captureOperationId: string): Promise<void> {
  validateMedia('MediaAdmission', admission); const engine = source.engine, definition = engine.sourceDefinition(admission.sourceId);
  check(definition.kind === 'email' && definition.accountId === admission.accountId && source.configuration.sourceId === admission.sourceId &&
    admission.producerPrincipalId === engine.settings.identity.principalId && definition.producerPrincipalIds.includes(admission.producerPrincipalId));
  const capture = await engine.find(admission.producerPrincipalId, captureOperationId), request = capture?.value.request;
  check(capture); if (!capture) throw Error('Unreachable');
  check(capture.value.phase === 'succeeded' && capture.value.action === 'capture' && capture.value.callerPrincipalId === admission.producerPrincipalId &&
    capture.value.ignoredReason === null && capture.value.effect?.objectId === admission.itemObjectId && request?.action === 'capture');
  if (request?.action !== 'capture') throw Error('Unreachable');
  check(request.sourceId === admission.sourceId && same(request.expectedScope, engine.settings.identity.scope) &&
    capture.value.requestHash === hashJson(request) && request.operationId === captureOperationId);
  const item = await engine.item(admission.itemObjectId);
  check(item.value.sourceId === admission.sourceId && item.value.message.attachments === 'expected' && same(item.value.message, request.message) &&
    item.value.identityHash === admission.identityHash && item.value.messageHash === admission.messageHash &&
    identityHash(admission.sourceId, request.message) === admission.identityHash && messageHash(request.message) === admission.messageHash &&
    request.message.accountId === admission.accountId && request.message.messageId === hashJson(['outlook-message', admission.providerMessageId]));
  check(request.message.attachments === 'expected');
}

export class MediaStore {
  constructor(readonly engine: SecretaryEngine, readonly admissionGuard: (admission: MediaAdmission, captureOperationId: string) => Promise<void>) {}
  private async named(key: string, name: string): Promise<Pin | null> {
    const page = await this.engine.client.request('objects.query', { contractKey: key, includeArchived: true, limit: 2,
      where: { op: 'and', args: [{ op: 'eq', field: 'object.parentId', value: this.engine.store.root }, { op: 'eq', field: 'object.name', value: name }] } });
    check(page.items.length <= 1 && !page.nextCursor); const value = page.items[0]; return value ? { objectId: value.objectId, revision: value.revision } : null;
  }
  private async read(key: string, name: string, pin: Pin) {
    const result = await this.engine.client.request('objects.read', pin);
    check(result.object.parentId === this.engine.store.root && result.object.name === name && result.object.contractKey === key && !result.object.effectivelyArchived &&
      pin.revision === 1 && result.object.currentRevision === 1 && result.revision.revision === 1 && result.revision.contractVersion === '1.0.0');
    return result;
  }
  private async chunk(pin: Pin, ownerObjectId: string, name?: string): Promise<Buffer> {
    const result = await this.engine.client.request('objects.read', pin);
    check(result.object.id === pin.objectId && result.object.ownerObjectId === ownerObjectId && result.object.contractKey === 'secretary/media-chunk' &&
      !result.object.effectivelyArchived && result.object.currentRevision === pin.revision && result.revision.contractVersion === '1.0.0' && (name === undefined || result.object.name === name));
    check(result.content.encoding === 'base64' && result.revision.mediaType === 'application/octet-stream');
    if (result.content.encoding !== 'base64') throw Error('Unreachable');
    const bytes = Buffer.from(result.content.value, 'base64'); check(bytes.length > 0 && bytes.length <= chunkBytes && bytes.length === result.revision.byteLength && digest(bytes) === result.revision.contentHash); return bytes;
  }
  private async write(key: string, name: string, content: Wire.Content, ownerObjectId: string, references: Wire.RevisionReferences = {}): Promise<Pin> {
    const old = await this.named(key, name); if (old) return old;
    await this.engine.verifyOwner();
    try {
      const result = await this.engine.client.request('objects.write', { mutationId: await import('../../../packages/sdk/src/node.js').then(({ newOperationId }) => newOperationId(this.engine.client)), references,
        contractVersion: '1.0.0', create: { parentId: this.engine.store.root, contractKey: key, name, ownerObjectId }, content });
      return { objectId: result.object.id, revision: result.revision.revision };
    } catch (error) { const saved = await this.named(key, name); if (!saved) throw error; return saved; }
  }
  async publish(spool: MediaSpool, operationId: string): Promise<{ pin: Pin; value: MediaManifest }> {
    const { attempt, bytes } = spool.payload(operationId), { intent, evidence } = attempt; check(evidence); if (!evidence) throw Error('Unreachable');
    await this.admissionGuard(intent.admission, intent.captureOperationId); const chunks: Pin[] = [];
    for (let offset = 0; offset < bytes.length; offset += chunkBytes) {
      const index = chunks.length, part = bytes.subarray(offset, offset + chunkBytes);
      const pin = await this.write('secretary/media-chunk', nameFor('chunk', intent, index), { encoding: 'base64', value: part.toString('base64') }, intent.admission.itemObjectId);
      check((await this.chunk(pin, intent.admission.itemObjectId, nameFor('chunk', intent, index))).equals(part)); chunks.push(pin);
    }
    const value: MediaManifest = { schemaVersion: 1, admission: intent.admission, evidence, chunks }; validateMedia('MediaManifest', value);
    const pin = await this.write('secretary/media-manifest', nameFor('manifest', intent), { encoding: 'json', value: value as unknown as Wire.Json }, intent.admission.itemObjectId,
      Object.fromEntries(chunks.map((pin, index) => ['chunk-' + index, pin])));
    const saved = await this.manifest(pin, intent.admission); check(same(saved, value)); return { pin, value: saved };
  }
  async manifest(pin: Pin, expected?: MediaAdmission): Promise<MediaManifest> {
    const result = await this.engine.client.request('objects.read', pin);
    check(result.object.id === pin.objectId && result.object.contractKey === 'secretary/media-manifest' && !result.object.effectivelyArchived &&
      result.object.currentRevision === pin.revision && result.revision.contractVersion === '1.0.0');
    check(result.content.encoding === 'json' && result.revision.mediaType === 'application/json'); if (result.content.encoding !== 'json') throw Error('Unreachable');
    const value = result.content.value as unknown as MediaManifest; validateMedia('MediaManifest', value);
    check((!expected || same(value.admission, expected)) && result.object.ownerObjectId === value.admission.itemObjectId && result.revision.contentHash === hashJson(value) &&
      result.revision.byteLength === Buffer.byteLength(canonical(value)) && value.evidence.attachment.messageId === value.admission.providerMessageId &&
      value.chunks.length === Math.ceil(value.evidence.byteLength / chunkBytes) && new Set(value.chunks.map(chunk => chunk.objectId)).size === value.chunks.length &&
      same(result.revision.references, Object.fromEntries(value.chunks.map((chunk, index) => ['chunk-' + index, chunk]))));
    return value;
  }
  async payload(pin: Pin, expected?: MediaAdmission): Promise<Buffer> {
    const value = await this.manifest(pin, expected), chunks: Buffer[] = [];
    for (const [index, chunkPin] of value.chunks.entries()) { const part = await this.chunk(chunkPin, value.admission.itemObjectId); check(part.length === Math.min(chunkBytes, value.evidence.byteLength - index * chunkBytes)); chunks.push(part); }
    const bytes = Buffer.concat(chunks, value.evidence.byteLength); check(digest(bytes) === value.evidence.contentHash); return bytes;
  }
  async coverage(item: Pin): Promise<{ pin: Pin; value: MediaCoverage } | null> {
    const name = 'media-coverage-' + item.objectId, pin = await this.named('secretary/media-coverage', name); if (!pin) return null;
    const result = await this.read('secretary/media-coverage', name, pin);
    check(result.content.encoding === 'json' && result.revision.mediaType === 'application/json'); if (result.content.encoding !== 'json') throw Error('Unreachable');
    const value = result.content.value as unknown as MediaCoverage; validateMedia('MediaCoverage', value);
    check(value.admission.itemObjectId === item.objectId && hashJson(value) === result.revision.contentHash && Buffer.byteLength(canonical(value)) === result.revision.byteLength &&
      same(result.revision.references, Object.fromEntries(value.attachments.flatMap((attachment, index) => attachment.manifest ? [['manifest-' + index, attachment.manifest] as const] : []))));
    for (const attachment of value.attachments) {
      if (!attachment.manifest) continue;
      const raw = await this.engine.client.request('objects.read', attachment.manifest);
      check(raw.content.encoding === 'json'); if (raw.content.encoding !== 'json') throw Error('Unreachable');
      const manifest = raw.content.value as unknown as MediaManifest; validateMedia('MediaManifest', manifest);
      check(same(manifest.admission, value.admission) && same(manifest.evidence.attachment, attachment.attachment));
      await this.manifest(attachment.manifest, value.admission);
    }
    return { pin, value };
  }
  async saveCoverage(value: MediaCoverage, plan: MediaReadPlan): Promise<{ pin: Pin; value: MediaCoverage }> {
    validateMedia('MediaCoverage', value); await this.admissionGuard(value.admission, plan.captureOperationId);
    check(same(value.admission, plan.admission));
    const pin = await this.write('secretary/media-coverage', 'media-coverage-' + value.admission.itemObjectId, { encoding: 'json', value: value as unknown as Wire.Json }, value.admission.itemObjectId,
      Object.fromEntries(value.attachments.flatMap((attachment, index) => attachment.manifest ? [['manifest-' + index, attachment.manifest] as const] : [])));
    const saved = await this.coverage(plan.item); check(saved && same(saved.value, value));
    const item = await this.engine.item(value.admission.itemObjectId);
    const media = { contractKey: 'secretary/media-coverage' as const, pin, contentHash: hashJson(value), name: 'Outlook attachments', mediaType: 'application/json', errorCode: null };
    if (!item.value.media.some(entry => same(entry, media))) {
      const request = { action: 'attach' as const, operationId: deriveOperationId(plan.captureOperationId, ['outlook-media', hashJson(value)]), expectedScope: this.engine.settings.identity.scope,
        sourceId: value.admission.sourceId, item: item.pin, media: [media] };
      const attached = await this.engine.action(value.admission.producerPrincipalId, request);
      check(attached.phase === 'succeeded' && attached.effect?.objectId === item.pin.objectId);
    }
    return saved!;
  }
}
