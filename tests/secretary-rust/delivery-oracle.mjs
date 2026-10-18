import assert from 'node:assert/strict';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const root = resolve(process.env.IVY_SECRETARY_ORACLE ?? '.local/build/secretary');
const load = path => import(pathToFileURL(resolve(root, path)));
const { discoverVoiceTarget, uniqueVoiceTarget } = await load('services/secretary/src/voice-target.js');
const { messageConversationUnread } = await load('services/secretary/src/assignment-message.js');
const { IvyError } = await load('packages/sdk/src/node.js');
const rows = [], dates = new Map();
function capture(value) {
  if (typeof value === 'string') dates.set(value, Number.isFinite(Date.parse(value)) ? Date.parse(value) : null);
  else if (value && typeof value === 'object') for (const child of Object.values(value)) capture(child);
}
async function add(input) {
  capture(input);
  const steps = structuredClone(input.steps ?? []), trace = [], results = [];
  const client = { async request(method, params, options) {
    trace.push({ method, params: structuredClone(params), timeout: options?.timeoutMs ?? 35000 });
    const step = steps.shift(); assert.ok(step, `Unexpected ${method}`); assert.equal(method, step.method);
    if (step.error) throw new IvyError(step.error.code, step.error.message, step.error.outcome);
    return structuredClone(step.value);
  } };
  for (let i = 0; i < (input.repeats ?? 1); i++) {
    try {
      const value = input.kind === 'voice' ? await discoverVoiceTarget(client)
        : input.kind === 'unique' ? uniqueVoiceTarget(input.candidates)
        : await messageConversationUnread({ client, settings: { identity: { scope: { rootObjectId: input.root } } } }, input.batch);
      results.push({ value });
    } catch (cause) {
      const e = IvyError.from(cause); results.push({ code: e.code, message: e.message, outcome: e.outcome });
    }
  }
  rows.push({ input, expected: { results, trace, remaining: steps.length } });
}
const node = id => ({ serviceName: 'phone-bridge', serviceNodeId: id, connected: true, synced: true, ready: true, stale: false });
const page = (items, nextCursor = null) => ({ method: 'serviceNodes.list', value: { items, nextCursor } });
const binding = (id, name, hash = 'sha256:' + 'a'.repeat(64), version = '1.0.0', read = true) => ({ method: 'tools.list', value: {
  items: [{ qualifiedName: name, definitionHash: hash, definition: { interfaceVersion: version, annotations: { readOnlyHint: read } } }],
  provider: { node: { serviceNodeId: id } }, nextCursor: null,
} });
const status = recipients => ({ method: 'tools.call', value: { registration: { state: 'registered' }, recipients } });
const candidate = (id = 'voice.one', recipients = ['recipient']) => [binding(id, 'phone.status'), status(recipients), binding(id, 'phone.request', 'sha256:' + 'b'.repeat(64), '1.0.0', false)];
const failure = (method, code = 'outcome_unknown', outcome = 'unknown') => ({ method, error: { code, message: 'fixture failure', outcome } });
for (const count of [0, 1, 2, 3]) {
  const ids = Array.from({ length: count }, (_, i) => 'voice.' + i);
  await add({ kind: 'voice', steps: [page(ids.map(node)), ...ids.flatMap(id => candidate(id))] });
  await add({ kind: 'unique', candidates: ids.map(serviceNodeId => ({ serviceNodeId, recipientId: 'r', requestDefinitionHash: 'h' })) });
}
await add({ kind: 'voice', steps: [page([], 'next'), page([node('voice.one')]), ...candidate()] });
await add({ kind: 'voice', steps: [page([node('voice.one')], 'next'), page([node('voice.two')]), ...candidate(), ...candidate('voice.two')] });
for (const field of ['connected', 'synced', 'ready', 'stale', 'serviceName']) {
  const skipped = { ...node('skip'), [field]: field === 'stale' ? true : field === 'serviceName' ? 'another-service' : false };
  await add({ kind: 'voice', steps: [page([skipped, node('voice.one')]), ...candidate()] });
}
for (const recipients of [[], ['one', 'two'], null, 'one']) {
  await add({ kind: 'voice', steps: [page([node('voice.one')]), binding('voice.one', 'phone.status'), status(recipients)] });
}
for (const recipient of ['', '\ud800', null, 1, { retained: true }]) await add({ kind: 'voice', steps: [page([node('voice.one')]), ...candidate('voice.one', [recipient])] });
for (const registration of [null, {}, { state: 'pending' }, false]) {
  await add({ kind: 'voice', steps: [page([node('voice.one')]), binding('voice.one', 'phone.status'), { method: 'tools.call', value: { registration, recipients: ['one'] } }] });
}
for (const value of [null, false, [], {}]) await add({ kind: 'voice', steps: [page([node('voice.one')]), binding('voice.one', 'phone.status'), { method: 'tools.call', value }] });
for (const outcome of ['not_executed', 'unknown', 'completed']) {
  await add({ kind: 'voice', steps: [failure('serviceNodes.list', 'fixture', outcome)] });
  for (const index of [0, 1, 2]) {
    const reads = candidate(); reads[index] = failure(reads[index].method, 'fixture', outcome);
    await add({ kind: 'voice', steps: [page([node('voice.one'), node('voice.two')]), ...reads.slice(0, index + 1), ...candidate('voice.two')] });
  }
}
await add({ kind: 'voice', steps: [page([node('voice.one')]), binding('other', 'phone.status')] });
await add({ kind: 'voice', steps: [page([node('voice.one')]), binding('voice.one', 'phone.status', 'h', '2.0.0')] });
await add({ kind: 'voice', repeats: 2, steps: [page([node('voice.one')]), ...candidate(), page([node('voice.one')]), status(['recipient'])] });
const due = '2026-01-01T00:00:00.000Z';
const batch = { kind: 'secretary.channel.batch', dueAt: due, serviceNodeId: 'message.source', channel: { reference: { account: 'fixture', nested: ['\ud800'] } }, itemIds: [], eventIds: [] };
const ready = { method: 'serviceNodes.get', value: node('message.source') };
const unread = value => [ready, binding('message.source', 'message-channel.unread'), { method: 'tools.call', value }];
for (const state of [true, false]) await add({ kind: 'unread', root: 'root', batch, steps: unread({ complete: true, unread: state, observedAt: due }) });
for (const field of ['connected', 'synced', 'ready', 'stale']) {
  await add({ kind: 'unread', root: 'root', batch, steps: [{ method: 'serviceNodes.get', value: { ...ready.value, [field]: field === 'stale' } }] });
}
for (const observation of [null, {}, { complete: false, unread: true, observedAt: due }, { complete: true, unread: null, observedAt: due },
  { complete: true, unread: true, observedAt: '2025-12-31T23:59:59.999Z' }, { complete: true, unread: true, observedAt: 'invalid' }]) {
  await add({ kind: 'unread', root: 'root', batch, steps: unread(observation) });
}
for (const outcome of ['not_executed', 'unknown', 'completed']) for (const index of [0, 1, 2]) {
  const steps = unread({ complete: true, unread: true, observedAt: due }); steps[index] = failure(steps[index].method, 'fixture', outcome);
  await add({ kind: 'unread', root: 'root', batch, steps: steps.slice(0, index + 1) });
}
await add({ kind: 'unread', root: '\ud800', batch: { ...batch, dueAt: 'invalid' }, steps: unread({ complete: true, unread: false, observedAt: due }) });
await add({ kind: 'unread', root: 'root', batch, repeats: 2, steps: [...unread({ complete: true, unread: true, observedAt: due }), ready, { method: 'tools.call', value: { complete: true, unread: false, observedAt: due } }] });
for (const outcome of ['not_executed', 'unknown', 'completed']) {
  const steps = [ready, binding('message.source', 'message-channel.unread'), failure('tools.call', 'tool_definition_changed', outcome)];
  if (outcome === 'not_executed') steps.push(binding('message.source', 'message-channel.unread', 'new-hash'), { method: 'tools.call', value: { complete: true, unread: true, observedAt: due } });
  await add({ kind: 'unread', root: 'root', batch, steps });
}
process.stdout.write(JSON.stringify({ dates: [...dates], rows }));
