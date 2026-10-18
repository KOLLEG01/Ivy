import {registerHooks, stripTypeScriptTypes} from 'node:module';
import {readFileSync, existsSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(specifier, context, next) {
  if (specifier.startsWith('.') && specifier.endsWith('.js') && context.parentURL) {
    const url = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
    if (existsSync(fileURLToPath(url))) return next(url.href, context);
  }
  return next(specifier, context);
}});
const {hashJson} = await import('../../packages/contracts/src/canonical.ts');
const {IvyError, requireThat} = await import('../../packages/contracts/src/errors.ts');
const source = readFileSync('services/task-board/src/runtime/store.ts', 'utf8');
const helpers = source.slice(source.indexOf('export const identityHash'), source.indexOf('export const taskAttachmentPins')).replaceAll('export ', '');
const {identityHash, mutation} = new Function('hashJson', stripTypeScriptTypes(helpers) + '\nreturn {identityHash,mutation};')(hashJson);
const method = source.slice(source.indexOf('  async allocateTaskKey('), source.indexOf('  private workspaceReservation('));
const sequenceKey = 'task-board/task-key-sequence', allocationKey = 'task-board/task-key-allocation';
const seq = value => ({schemaVersion: 1, nextValue: value, updatedAt: 'seed'});
const allocation = (id, taskKey) => ({schemaVersion: 1, operationId: id, taskKey, createdAt: 'seed'});
const name = id => 'Task key allocation ' + identityHash('task-board', id);
class Memory {
  docs = []; trace = []; receipts = new Map(); faults = [];
  constructor(root) { this.rootObjectId = root; }
  insert(key, label, value) {
    const pin = {objectId: 'doc' + (this.docs.length + 1), revision: 1};
    this.docs.push({key, name: label, pin, parentId: this.rootObjectId, value: structuredClone(value), contentHash: hashJson(value)});
    return pin;
  }
  hit(event, after, value, destination) {
    const index = this.faults.findIndex(f => f.event === event && !!f.after === after);
    if (index < 0) return;
    const fault = this.faults.splice(index, 1)[0];
    if (fault.winner) {
      const key = event === 'initialize' ? sequenceKey : allocationKey;
      const label = destination.create.name;
      const winner = event === 'initialize' ? seq(fault.winner.nextValue) : allocation(fault.winner.operationId ?? value.operationId, fault.winner.taskKey);
      this.insert(key, label, winner);
    }
    if (fault.advance) {
      const doc = this.docs.find(d => d.key === sequenceKey);
      doc.value.nextValue += fault.advance; doc.pin.revision++; doc.contentHash = hashJson(doc.value);
    }
    throw new IvyError(fault.code, 'Fixture ' + event + '.', fault.outcome ?? 'not_executed');
  }
  async named(key, label, parent) {
    this.trace.push(['named', key, label, parent ?? null]);
    const event = key === sequenceKey ? 'named-sequence' : 'named-allocation';
    this.hit(event, false);
    return structuredClone(this.docs.find(d => d.key === key && d.name === label && d.parentId === (parent ?? this.rootObjectId)) ?? null);
  }
  async read(key, address, upgrade = true) {
    this.trace.push(['read', key, address, upgrade]); this.hit('read', false);
    const doc = this.docs.find(d => d.key === key && d.pin.objectId === address.objectId && d.pin.revision === address.revision);
    if (!doc) throw new IvyError('not_found', 'Fixture document missing.');
    return structuredClone(doc);
  }
  async write(key, value, mutationId, destination) {
    this.trace.push(['write', key, value, mutationId, destination]);
    const event = key === allocationKey ? 'retain' : destination.create ? 'initialize' : 'increment';
    this.hit(event, false, value, destination);
    const hash = hashJson({key, value, destination});
    const receipt = this.receipts.get(mutationId);
    if (receipt) {
      requireThat(receipt.hash === hash, 'mutation_conflict', 'Fixture mutation identity conflict.');
      return structuredClone(receipt.pin);
    }
    let pin;
    if (destination.create) {
      requireThat(!this.docs.some(d => d.key === key && d.name === destination.create.name && d.parentId === destination.create.parentId), 'revision_conflict', 'Fixture name conflict.');
      pin = this.insert(key, destination.create.name, value);
    } else {
      const doc = this.docs.find(d => d.pin.objectId === destination.objectId);
      requireThat(doc && doc.pin.revision === destination.expectedRevision, 'revision_conflict', 'Fixture CAS conflict.');
      doc.pin.revision++; doc.value = structuredClone(value); doc.contentHash = hashJson(value); pin = structuredClone(doc.pin);
    }
    this.receipts.set(mutationId, {hash, pin: structuredClone(pin)});
    this.hit(event, true, value, destination);
    return structuredClone(pin);
  }
}
const allocate = id => ({allocate: id}), fault = (event, code, extra = {}) => ({fault: {event, code, ...extra}});
const scenarios = [
  ['fresh journal', [allocate('op')]],
  ['retained restart', [allocate('op'), allocate('op')]],
  ['independent identities', [allocate('a'), allocate('b'), allocate('a'), allocate('c')]],
  ['retained without sequence', [{allocation: {id: 'op', taskKey: 'TASK-0042'}}, allocate('op')]],
  ['retained wrong operation', [{allocation: {id: 'op', owner: 'other', taskKey: 'TASK-0042'}}, allocate('op')]],
  ['existing sequence', [{sequence: 41}, allocate('op')]],
  ['UTF16 identity', [allocate('op\ud800'), allocate('op\udfff'), allocate('op\ud800')]],
  ['null root', [allocate('op')], null],
  ['UTF16 root', [allocate('op')], 'root\ud800'],
  ['sequence read conflict', [fault('read', 'revision_conflict'), allocate('op')]],
  ['sequence read unavailable then restart', [fault('read', 'service_unavailable'), allocate('op'), allocate('op')]],
  ['allocation lookup unavailable', [fault('named-allocation', 'service_unavailable'), allocate('op')]],
  ['sequence lookup conflict is not retried', [fault('named-sequence', 'revision_conflict'), allocate('op')]],
];
for (const event of ['initialize', 'increment', 'retain']) {
  for (const code of ['revision_conflict', 'mutation_conflict']) {
    scenarios.push([event + ' lost conflict reply ' + code, [fault(event, code, {after: true}), allocate('op'), allocate('op')]]);
    scenarios.push([event + ' conflict before write ' + code, [fault(event, code), allocate('op'), allocate('op')]]);
  }
  for (const after of [false, true]) scenarios.push([event + ' unknown ' + after, [fault(event, 'outcome_unknown', {after, outcome: 'unknown'}), allocate('op'), allocate('op')]]);
  scenarios.push([event + ' fatal', [fault(event, 'schema_invalid'), allocate('op')]]);
}
scenarios.push(
  ['competing initializer', [fault('initialize', 'revision_conflict', {winner: {nextValue: 99}}), allocate('op')]],
  ['competing sequence increment', [{sequence: 7}, fault('increment', 'revision_conflict', {advance: 3}), allocate('op')]],
  ['competing allocation same operation', [fault('retain', 'mutation_conflict', {winner: {taskKey: 'TASK-0038'}}), allocate('op'), allocate('op')]],
  ['competing allocation wrong operation', [fault('retain', 'revision_conflict', {winner: {operationId: 'other', taskKey: 'TASK-0038'}}), allocate('op')]],
  ['competing allocation lookup unavailable', [fault('retain', 'mutation_conflict'), fault('named-allocation', 'service_unavailable', {armAfterFirst: true}), allocate('op')]],
);
for (const number of [998, 999, 9999, 9007199254740990]) scenarios.push(['numeric rendering ' + number, [{sequence: number}, allocate('op')]]);
const results = [];
for (const [label, steps, root = 'root'] of scenarios) {
  let ticks = 0;
  class FixedDate extends Date { constructor() { super(Date.parse('2026-01-02T03:04:05.000Z') + ticks++); } }
  const Allocator = new Function('IvyError', 'requireThat', 'identityHash', 'mutation', 'Date', stripTypeScriptTypes('class Allocator {\n' + method + '\n}') + '\nreturn Allocator;')(IvyError, requireThat, identityHash, mutation, FixedDate);
  const store = new Memory(root), outcomes = [];
  for (const step of steps) {
    if ('sequence' in step) { store.insert(sequenceKey, 'Task key sequence', seq(step.sequence)); continue; }
    if (step.allocation) { const a = step.allocation; store.insert(allocationKey, name(a.id), allocation(a.owner ?? a.id, a.taskKey)); continue; }
    if (step.fault) {
      // The second allocation lookup occurs only after a failed retention write.
      if (step.fault.armAfterFirst) { const hit = store.hit.bind(store); let first = true; store.hit = (event, ...args) => { if (event === 'named-allocation' && first) { first = false; return; } return hit(event, ...args); }; }
      store.faults.push(step.fault); continue;
    }
    try { outcomes.push(await Allocator.prototype.allocateTaskKey.call(store, step.allocate)); }
    catch (error) { outcomes.push({error: {code: error.code, message: error.message, outcome: error.outcome}}); }
  }
  results.push({name: label, root, steps, expected: {outcomes, trace: store.trace, documents: store.docs, receipts: [...store.receipts], ticks}});
}
process.stdout.write(JSON.stringify(results));
