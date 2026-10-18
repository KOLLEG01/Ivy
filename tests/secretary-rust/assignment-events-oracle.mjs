import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const root = resolve(process.env.IVY_SECRETARY_ORACLE ?? '.local/build/secretary');
const load = path => import(pathToFileURL(resolve(root, path)));
const object = await load('services/secretary/src/assignment-object.js');
const message = await load('services/secretary/src/assignment-message.js');
const { IvyError } = await load('packages/sdk/src/node.js');
const rows = [], dates = new Map();
function capture(value) {
  if (typeof value === 'string') dates.set(value, Number.isFinite(Date.parse(value)) ? Date.parse(value) : null);
  else if (value && typeof value === 'object') for (const item of Object.values(value)) capture(item);
}
function add(method, ...args) {
  capture(args);
  let expected;
  try {
    const input = structuredClone(args);
    if (method === 'observationReady') input[3] = new Date(input[3]);
    expected = { value: (object[method] ?? message[method])(...input) };
  } catch (cause) {
    const error = IvyError.from(cause);
    expected = { code: error.code, message: error.message, outcome: error.outcome };
  }
  rows.push({ method, args, expected });
}
const specimen = JSON.parse('{"a/b":{"~key":[null,{"x":7}]},"":{"":false},"a~1b":3,"__proto__":{"own":1},"constructor":2,"😀":"ok","\\ud800":4}');
const values = [specimen, null, false, 3, 'text', [], [null, { x: 1 }], { missing: null }];
const paths = ['', '/', '//', '/a~1b/~0key/1/x', '/a~01b', '/__proto__', '/__proto__/own', '/constructor',
  '/toString', '/😀', '/\ud800', '/missing', '/0', '/1/x', '/01', '/-0', '/+0', '/1.0', '/length', '/length/x',
  '/99999999999999999999999999', '/4294967295', 'x', 'xx', '/~2', '/~'];
for (const value of values) {
  for (const path of paths) add('pointer', value, path);
  for (const selected of [[], [''], ['/missing'], ['/missing', '/0'], paths, ['/__proto__', '/__proto__']]) {
    add('objectProjection', value, { paths: selected });
  }
}
for (const [left, right] of [[{}, {}], [{ a: 1, b: 2 }, { b: 2, a: 1 }], [null, {}],
  [{ a: null }, {}], [[1, 2], [2, 1]], [specimen, specimen], ['\ud800', '\ud801'], [0, -0]]) {
  add('projectionChanged', left, right);
}
const due = '2026-01-01T00:00:00.000Z', now = Date.parse(due) + 60000;
const observation = { completePath: '/complete', observedAtPath: '/seen', maximumAgeSeconds: 60 };
const times = [due, new Date(now - 60001).toISOString(), new Date(now - 60000).toISOString(),
  new Date(now + 5000).toISOString(), new Date(now + 5001).toISOString(), '2026-01-01',
  '2026-01-01T01:00:00+01:00', '01/01/2026', '2026-02-30T00:00:00Z', 'invalid', '', null, 1767225600000];
for (const seen of times) for (const complete of [true, false, 1, 'true', null]) {
  add('observationReady', { complete, seen }, { observation }, due, now);
}
for (const condition of [null, { ...observation, maximumAgeSeconds: 0 },
  { ...observation, completePath: '/absent' }, { ...observation, observedAtPath: '/absent' }]) {
  add('observationReady', { complete: true, seen: due }, { observation: condition }, due, now);
}
add('observationReady', { complete: true, seen: due }, { observation }, 'invalid', now);
add('observationReady', { complete: true, seen: due }, { observation }, due, null); // epoch clock
const channel = { key: 'conversation', reference: { scope: 'fixture' }, occurredAt: due, itemId: null };
for (const occurredAt of times) for (const itemId of [undefined, null, '', 'item', 1, {}]) {
  const value = { ...channel, occurredAt, itemId };
  if (itemId === undefined) delete value.itemId;
  add('messageChannel', { messageChannel: value });
}
for (const key of ['', 'a', '\ud800', 1, null]) for (const reference of [{}, [], null, 'value']) {
  add('messageChannel', { messageChannel: { ...channel, key, reference } });
}
for (const value of [null, [], {}, { messageChannel: [] }, { messageChannel: null }]) add('messageChannel', value);
const batch = { kind: 'secretary.channel.batch', dueAt: due, serviceNodeId: 'fixture.source', channel, itemIds: [], eventIds: [] };
for (const dueAt of [due, 'invalid', '', null]) for (const ids of [[], [''], ['a', 'a'], [1], null]) {
  add('messageBatch', { ...batch, dueAt, itemIds: ids, extra: 'retained' });
}
const omittedItem = structuredClone(batch); delete omittedItem.channel.itemId;
add('messageBatch', omittedItem);
add('messageBatch', { ...batch, kind: 'secretary.message.batch' });
add('messageBatch', { ...batch, serviceNodeId: '' });
add('messageBatch', { ...batch, eventIds: [1] });
for (const observedAt of times) for (const complete of [true, false]) for (const unread of [true, false, null]) {
  add('messageUnreadSnapshot', { complete, unread, observedAt }, batch);
}
add('messageUnreadSnapshot', { complete: true, unread: false, observedAt: due }, { ...batch, dueAt: 'invalid' });
// Retain exact date inputs and Node results alongside the original policy output
// so the Rust consumer can verify its shared parser without injecting Node values.
process.stdout.write(JSON.stringify({ dates: [...dates], rows }));
