import { readFileSync, writeFileSync } from 'node:fs';
const microsoft = JSON.parse(readFileSync('specs/schemas/secretary-microsoft.schema.json', 'utf8')).$defs;
const ref = name => ({ $ref: '#/$defs/' + name });
const object = properties => ({ type: 'object', properties, required: Object.keys(properties), additionalProperties: false });
const text = maximum => ({ type: 'string', minLength: 1, maxLength: maximum });
const nullable = value => ({ anyOf: [value, { type: 'null' }] });
const integer = (minimum, maximum) => ({ type: 'integer', minimum, maximum });
const hash = { type: 'string', pattern: '^sha256:[0-9a-f]{64}$' }, stamp = { type: 'string', format: 'date-time', maxLength: 40 };
const definitions = {
  OutlookCollector: microsoft.OutlookCollector, NativeTarget: microsoft.NativeTarget, MicrosoftProfile: microsoft.MicrosoftProfile,
  MicrosoftBinding: microsoft.MicrosoftBinding,
  MediaPin: microsoft.Pin,
  MediaAdmission: object({ sourceId: text(64), producerPrincipalId: text(256), accountId: text(256), providerMessageId: text(4096),
    itemObjectId: text(256), identityHash: hash, messageHash: hash }),
  MediaIntent: object({ operationId: text(256), captureOperationId: text(256), item: ref('MediaPin'), admission: ref('MediaAdmission'), materializationHash: hash }),
  MediaAttachment: object({ messageId: text(4096), attachmentId: text(4096), attachmentType: { enum: ['fileAttachment', 'itemAttachment', 'referenceAttachment', 'unknown'] },
    name: text(1024), mediaType: nullable(text(256)), providerSizeBytes: integer(0, 2147483647), inline: { type: 'boolean' }, contentId: nullable(text(4096)),
    modifiedAt: nullable(stamp), payloadFetchSupported: { type: 'boolean' } }),
  MediaMaterialization: object({ attachment: ref('MediaAttachment'), fileId: text(1024), downloadUrl: text(16384), nativeResponseHash: hash }),
  MediaDownloadEvidence: object({ schemaVersion: { const: 1 }, attachment: ref('MediaAttachment'), fileId: text(1024), materializationHash: hash, downloadUrlHash: hash,
    startedAt: stamp, observedAt: stamp, byteLength: integer(0, 33554432), contentHash: hash, providerSizeRelation: { enum: ['equal', 'different'] },
    http: object({ status: { const: 200 }, contentLength: nullable(integer(0, 33554432)), contentEncoding: nullable(text(256)), contentType: nullable(text(256)), contentMd5: nullable(text(256)) }) }),
  MediaManifest: object({ schemaVersion: { const: 1 }, admission: ref('MediaAdmission'), evidence: ref('MediaDownloadEvidence'), chunks: { type: 'array', maxItems: 32, items: ref('MediaPin') } }),
  MediaReadPlan: object({ schemaVersion: { const: 1 }, admission: ref('MediaAdmission'), item: ref('MediaPin'), captureOperationId: text(256), configuration: ref('OutlookCollector'), epoch: text(256), createdAt: stamp,
    bindings: { type: 'array', minItems: 4, maxItems: 4, items: ref('MicrosoftBinding') } }),
  MediaReadCall: object({ planHash: hash, slot: integer(0, 38), method: microsoft.MicrosoftBinding.properties.method,
    params: { type: 'object', additionalProperties: true }, operationId: text(256), definitionHash: hash, preparedAt: stamp }),
  MediaCallReceipt: object({ slot: integer(0, 38), operationId: text(256), method: microsoft.MicrosoftBinding.properties.method, contentHash: hash }),
  MediaCoverage: object({ schemaVersion: { const: 1 }, admission: ref('MediaAdmission'), observedAt: stamp, gap: nullable(text(128)),
    attachments: { type: 'array', maxItems: 32, items: object({ attachment: ref('MediaAttachment'), manifest: nullable(ref('MediaPin')), errorCode: nullable(text(128)) }) } }),
};
const path = 'specs/schemas/secretary-media.schema.json';
const bytes = JSON.stringify({ $schema: 'https://json-schema.org/draft/2020-12/schema', $defs: definitions }, null, 2) + '\n';
if (process.argv.includes('--check')) { if (readFileSync(path, 'utf8') !== bytes) throw Error('Stale Secretary media contract.'); }
else writeFileSync(path, bytes);
