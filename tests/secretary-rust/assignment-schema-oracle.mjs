import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createRequire } from 'node:module';
import { schemaSamples } from '../fixtures/schema-samples.mjs';
const root = resolve(process.env.IVY_SECRETARY_ORACLE ?? '.local/build/secretary');
const original = await import(pathToFileURL(resolve(root, 'services/secretary/src/assignment-schema.js')));
const { IvyError } = await import(pathToFileURL(resolve(root, 'packages/sdk/src/node.js')));
const rows = [];
function add(method, ...args) {
  let expected;
  try { expected = { value: original[method](...structuredClone(args)) ?? null }; }
  catch (cause) { const error = IvyError.from(cause); expected = { code: error.code, message: error.message, outcome: error.outcome }; }
  rows.push({ method, args, expected });
}
const window = { timeZone: 'Europe/Berlin', start: '08:00', end: '20:00' };
const rules = { main: { enabled: true, minimumUrgency: 'normal', window },
  whatsapp: { enabled: false, minimumUrgency: 'high', window: null, questionsOnly: true },
  voice: { enabled: true, minimumUrgency: 'critical', window: null, immediateOnly: true }, researchMaxMinutes: 3 };
const schedule = { kind: 'schedule', timeZone: 'Europe/Berlin', cadence: 'daily', intervalMinutes: null, localTime: '09:30', weekdays: [] };
const event = { kind: 'event', topic: 'example.event', topicVersion: '1.0.0', sourceServiceNodeId: null, eventKind: null };
const object = { kind: 'object-change', objectIds: ['00000000-0000-4000-8000-000000000001'], paths: [], delaySeconds: 30, observation: null };
const assignment = { schemaVersion: 1, assignmentId: 'example', name: 'Review', description: '', enabled: true,
  prompt: 'Review the retained state.', preflight: null, rules: {}, trigger: schedule,
  createdAt: '2026-01-01T00:00:00.000Z', updatedAt: '2026-01-01T00:00:00.000Z' };
const methods = { GlobalRules: 'validateRules', AssignmentTrigger: 'validateTrigger', Assignment: 'validateAssignment' };
const { Ajv2020 } = createRequire(import.meta.url)('ajv/dist/2020.js');
const document = JSON.parse(readFileSync('specs/schemas/secretary.schema.json', 'utf8'));
const ajv = new Ajv2020({ strict: false, validateFormats: false, ownProperties: true });
ajv.addSchema(document);
for (const row of schemaSamples(ajv, [document])) {
  const method = methods[row.reference.split('#/$defs/')[1]];
  if (method) {
    const value = JSON.parse(row.input);
    // Nonfinite values cannot be carried in the outer JSON fixture. Shared
    // canonical tests own those non-wire values; include the safe-number boundary.
    if (typeof value !== 'number' || Number.isFinite(value)) add(method, value);
  }
}
for (const timeZone of ['UTC', 'utc', 'Europe/Berlin', 'europe/berlin', 'America/New_York', 'Asia/Kathmandu',
  'Australia/Lord_Howe', 'Etc/GMT+5', 'US/Eastern', 'GMT', '+01', '-0430', '+05:45', '-00:00',
  '+23:59', '+24:00', '+1', 'Not/AZone', '', ' UTC ', 'UTC\ud800']) {
  add('validateTrigger', { ...schedule, timeZone });
  for (const channel of ['main', 'whatsapp', 'voice']) {
    add('validateRules', { ...rules, [channel]: { ...rules[channel], window: { ...window, timeZone } } });
  }
}
for (const cadence of ['interval', 'daily', 'weekly']) {
  for (const intervalMinutes of [null, 15]) for (const localTime of [null, '09:30']) {
    for (const weekdays of [[], [1], [1, 1], [0, 6], [7]]) {
      add('validateTrigger', { ...schedule, cadence, intervalMinutes, localTime, weekdays });
    }
  }
}
add('validateTrigger', event);
for (const paths of [[], [''], ['/'], ['/a'], ['/a~0b'], ['/a~1b'], ['/a~01'], ['/a~10'],
  ['/a~'], ['/a~2'], ['a'], ['/a', '/a'], ['/~00'], ['/\ud800'], ['/😀'], ['/constructor'], ['/__proto__']]) {
  add('validateTrigger', { ...object, paths });
  add('validateTrigger', { ...object, observation: { completePath: paths[0] ?? '', observedAtPath: '/seen', maximumAgeSeconds: 60 } });
}
add('validateTrigger', { ...object, objectIds: [...object.objectIds, ...object.objectIds] });
for (const [channel, rule] of Object.entries(rules).filter(([, v]) => typeof v === 'object')) {
  for (const selected of [null, { ...window, start: '09:00', end: '09:00' }, { ...window, timeZone: 'Not/AZone' }]) {
    add('validateRules', { ...rules, [channel]: { ...rule, window: selected } });
    // Assignment validation deliberately checks Main and Voice semantic windows,
    // while the retained legacy WhatsApp override is ignored when merging.
    add('validateAssignment', { ...assignment, trigger: event, rules: { [channel]: { window: selected } } });
  }
}
for (const assignmentId of ['', 'x'.repeat(256), 'x'.repeat(257), '😀'.repeat(128), '😀'.repeat(129), '\ud800', 'e\u0301']) {
  add('validateAssignment', { ...assignment, assignmentId });
  add('assignmentName', assignmentId);
  for (const key of ['', 'event:1', '😀', '\udfff']) add('executionName', assignmentId, key);
}
add('unifiedMainRules', rules);
for (const length of [1000000, 1048576]) add('validateAssignment', { ...assignment, extra: 'x'.repeat(length) });
for (const overrides of [{}, { whatsapp: { enabled: false, questionsOnly: true } }, { main: { minimumUrgency: 'critical' } },
  { main: { enabled: false }, whatsapp: { enabled: true } }, { voice: { enabled: false } },
  { main: { window: null } }, { main: { window: { ...window, start: '22:00', end: '06:00' } } },
  { main: { window: { ...window, start: '08:00', end: '08:00' } } }, { main: { unknown: true } },
  { researchMaxMinutes: null }, { researchMaxMinutes: 0 }, { researchMaxMinutes: 4 }, { researchMaxMinutes: 999 },
  { voice: { minimumUrgency: 'normal' } }, { main: null }, { voice: null }]) {
  add('unifiedRuleOverrides', overrides);
  add('effectiveRules', rules, overrides);
}
process.stdout.write(JSON.stringify(rows));
