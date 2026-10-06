import { SchemaValidators } from '../../../packages/sdk/src/node.js';
import schema from '../../../specs/schemas/secretary-media.schema.json' with { type: 'json' };
import { need } from './schema.js';
import type { Pin } from './schema.js';
import type { OutlookCollector, MicrosoftBinding, MicrosoftMethod } from './microsoft-schema.js';
import type { Wire } from '../../../packages/sdk/src/node.js';
export interface MediaAdmission { sourceId: string; producerPrincipalId: string; accountId: string; providerMessageId: string;
  itemObjectId: string; identityHash: string; messageHash: string }
export interface MediaIntent { operationId: string; captureOperationId: string; item: Pin; admission: MediaAdmission; materializationHash: string }
export interface MediaAttachment { messageId: string; attachmentId: string; attachmentType: 'fileAttachment' | 'itemAttachment' | 'referenceAttachment' | 'unknown';
  name: string; mediaType: string | null; providerSizeBytes: number; inline: boolean; contentId: string | null; modifiedAt: string | null; payloadFetchSupported: boolean }
/** Contains a capability URL. Local/private memory only, never a Hive contract or diagnostic. */
export interface MediaMaterialization { attachment: MediaAttachment; fileId: string; downloadUrl: string; nativeResponseHash: string }
export interface MediaDownloadEvidence { schemaVersion: 1; attachment: MediaAttachment; fileId: string; materializationHash: string; downloadUrlHash: string;
  startedAt: string; observedAt: string; byteLength: number; contentHash: string; providerSizeRelation: 'equal' | 'different';
  http: { status: 200; contentLength: number | null; contentEncoding: string | null; contentType: string | null; contentMd5: string | null } }
const validators = new SchemaValidators();
export interface MediaManifest { schemaVersion: 1; admission: MediaAdmission; evidence: MediaDownloadEvidence; chunks: Pin[] }
export interface MediaReadPlan { schemaVersion: 1; admission: MediaAdmission; item: Pin; captureOperationId: string; configuration: OutlookCollector; epoch: string; createdAt: string; bindings: MicrosoftBinding[] }
export interface MediaReadCall { planHash: string; slot: number; method: MicrosoftMethod; params: Record<string, Wire.Json>; operationId: string; definitionHash: string; preparedAt: string }
export interface MediaCoverage { schemaVersion: 1; admission: MediaAdmission; observedAt: string; gap: string | null;
  attachments: { attachment: MediaAttachment; manifest: Pin | null; errorCode: string | null }[] }
export function validateMedia(name: 'MediaAttachment' | 'MediaMaterialization' | 'MediaDownloadEvidence' | 'MediaAdmission' | 'MediaIntent' | 'MediaManifest' | 'MediaReadPlan' | 'MediaReadCall' | 'MediaCoverage', value: unknown): void {
  validators.validate({ ...schema, $ref: '#/$defs/' + name }, value);
  if (name === 'MediaAdmission' || name === 'MediaIntent') return;
  if (name === 'MediaReadPlan' || name === 'MediaReadCall') {
    const stamp = name === 'MediaReadPlan' ? (value as MediaReadPlan).createdAt : (value as MediaReadCall).preparedAt;
    need(Number.isFinite(Date.parse(stamp)) && new Date(stamp).toISOString() === stamp, 'media_timestamp_invalid', 'Native media work needs its original real UTC timestamp.'); return;
  }
  if (name === 'MediaCoverage') {
    const coverage = value as MediaCoverage;
    need(coverage.attachments.every(value => value.manifest ? value.errorCode === null && coverage.gap === null : value.errorCode !== null) &&
      new Set(coverage.attachments.map(value => value.attachment.attachmentId)).size === coverage.attachments.length &&
      Number.isFinite(Date.parse(coverage.observedAt)) && new Date(coverage.observedAt).toISOString() === coverage.observedAt,
      'media_coverage_invalid', 'Coverage retains one complete payload or an explicit error for each original attachment.');
    coverage.attachments.forEach(value => validateMedia('MediaAttachment', value.attachment)); return;
  }
  if (name === 'MediaManifest') { validateMedia('MediaDownloadEvidence', (value as MediaManifest).evidence); return; }
  const timestamp = (value: string | null) => {
    if (value === null) return;
    const match = /^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d{1,7}))?Z$/.exec(value);
    const normalized = match ? match[1] + '.' + (match[2] ?? '').padEnd(3, '0').slice(0, 3) + 'Z' : '';
    need(match && Number.isFinite(Date.parse(normalized)) && new Date(normalized).toISOString() === normalized,
      'media_timestamp_invalid', 'Media evidence needs a real UTC timestamp; its original precision is retained.');
  };
  const record = value as MediaAttachment & MediaMaterialization & MediaDownloadEvidence;
  timestamp(name === 'MediaAttachment' ? record.modifiedAt : record.attachment.modifiedAt);
  if (name === 'MediaDownloadEvidence') {
    timestamp(record.startedAt); timestamp(record.observedAt);
    need(record.providerSizeRelation === (record.attachment.providerSizeBytes === record.byteLength ? 'equal' : 'different') &&
      (record.http.contentLength === null || record.http.contentLength === record.byteLength) &&
      (record.http.contentEncoding === null || record.http.contentEncoding.toLowerCase() === 'identity'),
      'media_evidence_invalid', 'Media evidence must describe the complete acquired body.');
  }
}
