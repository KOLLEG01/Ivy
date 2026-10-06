import { readFileSync, writeFileSync } from 'node:fs';
const secretary = JSON.parse(readFileSync('specs/schemas/secretary.schema.json', 'utf8')).$defs;
const native = secretary.NativeTarget;
const ref = name => ({ $ref: '#/$defs/' + name });
const object = properties => ({ type: 'object', properties, required: Object.keys(properties), additionalProperties: false });
const text = (maxLength = 256) => ({ type: 'string', minLength: 1, maxLength });
const integer = (minimum, maximum) => ({ type: 'integer', minimum, maximum });
const nullable = value => ({ anyOf: [value, { type: 'null' }] });
const array = (items, minItems, maxItems) => ({ type: 'array', items, minItems, maxItems });
const stamp = { type: 'string', pattern: '^\\d{4}-\\d\\d-\\d\\dT\\d\\d:\\d\\d:\\d\\d\\.\\d{3}Z$' };
const hash = { type: 'string', pattern: '^sha256:[0-9a-f]{64}$' };
const json = { type: 'object', additionalProperties: true };
const method = { enum: ['thread/start', 'thread/resume', 'turn/start', 'thread/unsubscribe'] };
const defs = {
  Pin: secretary.Pin, Message: secretary.Message, Assessment: secretary.Assessment, Identity: secretary.Identity,
  Scope: secretary.Scope, Policy: secretary.Policy, SourceDefinition: secretary.SourceDefinition, NativeTarget: native,
  TriageSettings: object({ schemaVersion: { const: 1 }, target: ref('NativeTarget'), model: text(128),
    effort: { enum: ['low', 'medium', 'high', 'xhigh'] }, instructions: text(32768), threadCwd: text(2048), maximumConcurrent: integer(1, 8) }),
  TriageInput: object({ item: ref('Pin'), source: ref('SourceDefinition'), message: ref('Message'),
    contentScope: { enum: ['captured_text_or_caption', 'verified_media'] }, media: nullable(ref('TriageMedia')), policy: ref('Policy') }),
  TriageMediaAttachment: object({ name: nullable({ type: 'string', maxLength: 4096 }), mediaType: nullable(text(1024)), manifest: nullable(ref('Pin')), byteLength: nullable(integer(0, 33554432)),
    contentHash: nullable(hash), representation: { enum: ['original_bytes', 'rendered_preview'] }, kind: { enum: ['text', 'image', 'audio', 'video', 'unavailable'] }, inputIndex: nullable(integer(1, 352)), errorCode: nullable(text(128)) }),
  TriageMedia: object({ coverage: ref('Pin'), coverageContract: text(192), coverageHash: hash, gap: nullable(text(128)), attachments: array(ref('TriageMediaAttachment'), 0, 32) }),
  TriageBinding: object({ method, definitionHash: hash, outputSchema: json }),
  TriagePlan: object({ schemaVersion: { const: 1 }, identity: ref('Identity'), settings: ref('TriageSettings'), input: ref('TriageInput'),
    prompt: text(262144), promptHash: hash, developerInstructions: text(65536), outputSchema: json,
    nativeParams: ref('Pin'),
    bindings: array(ref('TriageBinding'), 4, 4), createdAt: stamp }),
  TriageThread: object({ schemaVersion: { const: 1 }, plan: nullable(ref('Pin')), threadId: nullable(text()) }),
  TriageWork: object({ schemaVersion: { const: 1 }, item: ref('Pin'), plan: ref('Pin'), operationId: text(), phase: { enum: ['pending', 'assessed', 'superseded', 'failed'] },
    completion: nullable(ref('Pin')), assessment: nullable(ref('Pin')), errorCode: nullable(text(128)) }),
  TriageCall: object({ schemaVersion: { const: 1 }, plan: ref('Pin'), slot: text(), method, request: ref('Pin'), operationId: text(),
    definitionHash: hash, preparedAt: stamp, observation: nullable(ref('Pin')) }),
  TriageCompletion: object({ schemaVersion: { const: 1 }, plan: ref('Pin'), turn: ref('Pin'), threadId: text(), turnId: text(),
    full: ref('Pin'), verification: ref('Pin'), result: ref('Assessment'), completedAt: stamp }),
};
const path = 'specs/schemas/secretary-triage.schema.json';
const bytes = JSON.stringify({ $schema: 'https://json-schema.org/draft/2020-12/schema', $defs: defs }, null, 2) + '\n';
if (process.argv.includes('--check')) { if (readFileSync(path, 'utf8') !== bytes) throw Error('Stale native Secretary triage contract.'); }
else writeFileSync(path, bytes);
