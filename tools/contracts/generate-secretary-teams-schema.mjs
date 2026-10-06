import { readFileSync, writeFileSync } from 'node:fs';
const shared = JSON.parse(readFileSync('specs/schemas/secretary-microsoft.schema.json', 'utf8')).$defs;
const ref = name => ({ $ref: '#/$defs/' + name });
const obj = properties => ({ type: 'object', properties, required: Object.keys(properties), additionalProperties: false });
const text = maximum => ({ type: 'string', minLength: 1, maxLength: maximum });
const integer = (minimum, maximum) => ({ type: 'integer', minimum, maximum });
const nullable = value => ({ anyOf: [value, { type: 'null' }] });
const array = (items, maxItems, minItems = 0) => ({ type: 'array', items, minItems, maxItems });
const stamp = shared.OutlookSelection.properties.since, hash = shared.OutlookCollector.properties.expectedAccountHash;
const defs = Object.fromEntries(['Message', 'Pin', 'SourceDefinition', 'NativeTarget', 'MicrosoftProfile', 'MicrosoftBinding', 'MicrosoftBlob'].map(name => [name, shared[name]]));
Object.assign(defs, {
  TeamsChat: obj({ id: text(4096), title: { type: 'string', maxLength: 1024 } }),
  TeamsSelection: obj({ since: stamp, until: stamp, chat: nullable(ref('TeamsChat')) }),
  TeamsCollector: obj({ sourceId: text(64), target: ref('NativeTarget'), expectedAccountHash: hash, profile: ref('MicrosoftProfile'), since: stamp,
    maximumChats: integer(1, 32), maximumMessages: integer(1, 32), excludedChatIds: { ...array(text(4096), 256), uniqueItems: true }, pollMs: integer(60000, 86400000), threadCwd: text(2048) }),
  TeamsConfiguration: obj({ schemaVersion: { const: 1 }, collectors: array(ref('TeamsCollector'), 32) }),
  TeamsPage: obj({ schemaVersion: { const: 1 }, sourceId: text(64), accountId: text(256), observedAt: stamp, selection: ref('TeamsSelection'), chats: array(ref('TeamsChat'), 32), messages: array(ref('Message'), 32),
    excluded: obj(Object.fromEntries(['hiddenChat', 'configuredChat', 'ownSender', 'senderNotAllowed', 'deleted', 'system', 'outsideWindow'].map(key => [key, integer(0, 32)]))), messagesWithAttachments: integer(0, 32) }),
  TeamsContinuation: obj({ selection: ref('TeamsSelection'), remainingChats: array(ref('TeamsChat'), 31) }),
  TeamsHead: obj({ schemaVersion: { const: 1 }, configuration: ref('TeamsCollector'), source: ref('SourceDefinition'), pending: nullable(ref('Pin')), lastPage: nullable(ref('Pin')),
    continuation: nullable(ref('TeamsContinuation')), nextPollAt: nullable(stamp), lastCompleteAt: nullable(stamp) }),
  TeamsPlan: obj({ schemaVersion: { const: 1 }, head: ref('Pin'), configuration: ref('TeamsCollector'), source: ref('SourceDefinition'), sourceCheckpoint: ref('Pin'), previousCursor: nullable(text(4096)),
    selection: ref('TeamsSelection'), epoch: text(256), preparedAt: stamp, bindings: array(ref('MicrosoftBinding'), 4, 4) }),
  TeamsCall: shared.OutlookCall,
  TeamsEvidence: { ...shared.OutlookEvidence, properties: { ...shared.OutlookEvidence.properties, page: nullable(ref('TeamsPage')) } },
  TeamsPublication: shared.OutlookPublication,
});
const path = 'specs/schemas/secretary-teams.schema.json', bytes = JSON.stringify({ $schema: 'https://json-schema.org/draft/2020-12/schema', $defs: defs }, null, 2) + '\n';
if (process.argv.includes('--check')) { if (readFileSync(path, 'utf8') !== bytes) throw Error('Stale Teams source contract.'); }
else writeFileSync(path, bytes);
