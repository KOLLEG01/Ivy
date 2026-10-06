import { readFileSync, writeFileSync } from 'node:fs';
const secretary = JSON.parse(readFileSync('specs/schemas/secretary.schema.json', 'utf8')).$defs;
const native = secretary.NativeTarget;
const ref = name => ({ $ref: '#/$defs/' + name });
const obj = properties => ({ type: 'object', properties, required: Object.keys(properties), additionalProperties: false });
const text = maximum => ({ type: 'string', minLength: 1, maxLength: maximum });
const integer = (minimum, maximum) => ({ type: 'integer', minimum, maximum });
const nullable = value => ({ anyOf: [value, { type: 'null' }] });
const stamp = { type: 'string', pattern: '^\\d{4}-\\d\\d-\\d\\dT\\d\\d:\\d\\d:\\d\\d\\.\\d{3}Z$' };
const hash = { type: 'string', pattern: '^sha256:[0-9a-f]{64}$' };
const array = (items, maxItems, minItems = 0) => ({ type: 'array', items, minItems, maxItems });
const method = { enum: ['account/read', 'thread/start', 'mcpServer/tool/call', 'thread/unsubscribe'] };
const defs = {
  Message: secretary.Message, Pin: secretary.Pin, SourceDefinition: secretary.SourceDefinition, NativeTarget: native,
  MicrosoftProfile: obj({ id: text(256), email: text(256) }),
  OutlookSelection: obj({ since: stamp, until: stamp, skip: integer(0, 1000000), pageSize: integer(1, 32) }),
  OutlookPage: obj({ schemaVersion: { const: 1 }, sourceId: text(64), accountId: text(256), observedAt: stamp, selection: ref('OutlookSelection'),
    nextIndex: nullable(integer(1, 1000000)), complete: { type: 'boolean' }, messages: { type: 'array', items: ref('Message'), maxItems: 32 },
    excluded: obj({ ownSender: integer(0, 32), senderNotAllowed: integer(0, 32) }), messagesWithAttachments: integer(0, 32) }),
  OutlookCollector: obj({ sourceId: text(64), target: ref('NativeTarget'), expectedAccountHash: hash, profile: ref('MicrosoftProfile'), since: stamp, pageSize: integer(1, 32), pollMs: integer(60000, 86400000), maximumPages: integer(1, 10000), threadCwd: text(2048) }),
  MicrosoftConfiguration: obj({ schemaVersion: { const: 1 }, collectors: array(ref('OutlookCollector'), 32) }),
  OutlookContinuation: obj({ selection: ref('OutlookSelection'), pageNumber: integer(2, 10000) }),
  OutlookHead: obj({ schemaVersion: { const: 1 }, configuration: ref('OutlookCollector'), source: ref('SourceDefinition'), pending: nullable(ref('Pin')), lastPage: nullable(ref('Pin')),
    continuation: nullable(ref('OutlookContinuation')), nextPollAt: nullable(stamp), lastCompleteAt: nullable(stamp) }),
  MicrosoftBinding: obj({ method, definitionHash: hash, outputSchema: { type: 'object', additionalProperties: true } }),
  OutlookPlan: obj({ schemaVersion: { const: 1 }, head: ref('Pin'), configuration: ref('OutlookCollector'), source: ref('SourceDefinition'), sourceCheckpoint: ref('Pin'), previousCursor: nullable(text(4096)),
    selection: ref('OutlookSelection'), pageNumber: integer(1, 10000), epoch: text(256), preparedAt: stamp, bindings: array(ref('MicrosoftBinding'), 4, 4) }),
  MicrosoftBlob: obj({ byteLength: integer(1, 33554432), contentHash: hash, chunks: array(ref('Pin'), 32, 1) }),
  OutlookCall: obj({ schemaVersion: { const: 1 }, plan: ref('Pin'), slot: integer(0, 6), method, params: { type: 'object', additionalProperties: true }, operationId: text(256), definitionHash: hash, preparedAt: stamp, seen: nullable(ref('MicrosoftBlob')), observation: nullable(ref('MicrosoftBlob')) }),
  OutlookEvidence: obj({ schemaVersion: { const: 1 }, plan: ref('Pin'), calls: array(ref('Pin'), 7, 1), observedAt: stamp, gap: nullable(text(128)), retryAfterSeconds: nullable(integer(1, 86400)), page: nullable(ref('OutlookPage')) }),
  OutlookPublication: obj({ schemaVersion: { const: 1 }, evidence: ref('Pin'), captured: array(ref('Pin'), 32), checkpoint: nullable(ref('Pin')) }),
};
const path = 'specs/schemas/secretary-microsoft.schema.json';
const bytes = JSON.stringify({ $schema: 'https://json-schema.org/draft/2020-12/schema', $defs: defs }, null, 2) + '\n';
if (process.argv.includes('--check')) { if (readFileSync(path, 'utf8') !== bytes) throw Error('Stale native Microsoft source contract.'); }
else writeFileSync(path, bytes);
