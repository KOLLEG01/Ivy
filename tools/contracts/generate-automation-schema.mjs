import { readFileSync, writeFileSync } from 'node:fs';
const schemaId = 'https://ivy.invalid/schemas/automation.schema.json';
const ref = name => ({ $ref: '#/$defs/' + name });
const wire = name => ({ $ref: 'https://ivy.invalid/schemas/hive-wire.schema.json#/$defs/' + name });
const object = properties => ({ type: 'object', properties, required: Object.keys(properties), additionalProperties: false });
const nullable = schema => ({ anyOf: [schema, { type: 'null' }] });
const integer = (minimum, maximum) => ({ type: 'integer', minimum, maximum });
const string = (minLength, maxLength, pattern) => ({ type: 'string', minLength, maxLength, ...(pattern ? { pattern } : {}) });
const defs = {
  Date: string(10, 10, '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'),
  LocalTime: string(5, 5, '^([01][0-9]|2[0-3]):[0-5][0-9]$'),
  Timestamp: string(20, 24, '^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}(?:\\.\\d{3})?Z$'),
  Definition: object({ scheduleId: string(1, 80, '^[a-z][a-z0-9-]*$'), principalId: wire('Identifier'),
    rootObjectId: nullable(wire('Identifier')), timeZone: string(1, 128), firstDate: ref('Date'), localTime: ref('LocalTime'),
    catchUp: { enum: ['all', 'latest'] }, subscriptionName: string(1, 128),
    inputSource: string(9, 256, '^service:.+$'), inputTopic: string(1, 192), initialSequence: integer(0, 9007199254740991) }),
  Settings: object({ definition: ref('Definition'), maximumPeriodsPerTick: integer(1, 10), pollMs: integer(1000, 60000) }),
  Pending: object({ periodDate: ref('Date'), advanceToDate: ref('Date'),
    skippedFromDate: nullable(ref('Date')), skippedThroughDate: nullable(ref('Date')),
    snapshot: nullable({ $ref: 'https://ivy.invalid/schemas/hive-operations.schema.json#/$defs/Status' }) }),
  Checkpoint: object({ schemaVersion: { const: 1 }, definition: ref('Definition'), definitionHash: wire('Hash'),
    nextDate: ref('Date'), pending: nullable(ref('Pending')) }),
  Period: object({ schemaVersion: { const: 1 }, definitionHash: wire('Hash'), periodDate: ref('Date'),
    timeZone: string(1, 128), localTime: ref('LocalTime'), skippedFromDate: nullable(ref('Date')), skippedThroughDate: nullable(ref('Date')),
    snapshot: { $ref: 'https://ivy.invalid/schemas/hive-operations.schema.json#/$defs/Status' } }),
  EventReceipt: object({ schemaVersion: { const: 1 }, definitionHash: wire('Hash'), sequence: integer(1, 9007199254740991),
    topic: string(1, 192), topicVersion: wire('ContractVersion'), source: string(9, 256, '^service:.+$'),
    occurredAt: ref('Timestamp'), sourceMutationId: wire('Identifier'), payloadHash: wire('Hash') }),
};
const schema = { $schema: 'https://json-schema.org/draft/2020-12/schema', $id: schemaId,
  title: 'Ivy SDK automation example contracts', $defs: defs };
const catalog = { schemaVersion: 1, schema: schemaId, contracts: [
  { key: 'automation-example/checkpoint', definition: '#/$defs/Checkpoint' },
  { key: 'automation-example/period', definition: '#/$defs/Period' },
  { key: 'automation-example/event-receipt', definition: '#/$defs/EventReceipt' },
] };
for (const [path, value] of [['specs/schemas/automation.schema.json', schema], ['specs/schemas/automation.contracts.json', catalog]]) {
  const text = JSON.stringify(value, null, 2) + '\n';
  if (process.argv.includes('--check')) { if (readFileSync(path, 'utf8') !== text) throw new Error('Stale automation contract: ' + path); }
  else writeFileSync(path, text);
}
