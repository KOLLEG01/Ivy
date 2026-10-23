import {registerHooks, stripTypeScriptTypes} from 'node:module';
import {existsSync, readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(s, c, next) {
  if (s.startsWith('.') && s.endsWith('.js') && c.parentURL) { const u = new URL(s.slice(0, -3) + '.ts', c.parentURL); if (existsSync(fileURLToPath(u))) return next(u.href, c); }
  return next(s, c);
}});
const {canonical, hashJson} = await import('../../packages/contracts/src/canonical.ts');
const {IvyError, requireThat} = await import('../../packages/contracts/src/errors.ts');
const storeSource = readFileSync('services/task-board/src/runtime/store.ts', 'utf8');
const helpers = storeSource.slice(storeSource.indexOf('export const identityHash'), storeSource.indexOf('export const taskAttachmentPins')).replaceAll('export ', '');
const {identityHash, mutation} = new Function('hashJson', stripTypeScriptTypes(helpers) + '\nreturn {identityHash,mutation};')(hashJson);
const operationsSource = readFileSync('services/task-board/src/runtime/operations.ts', 'utf8').replace(/^import [\s\S]*?;\r?\n/gm, '').replace('export class', 'class');
class FixedDate extends Date { constructor() { super('2026-01-02T03:04:05.000Z'); } }
const Operations = new Function('canonical', 'hashJson', 'IvyError', 'requireThat', 'identityHash', 'mutation', 'contractVersion', 'Date', stripTypeScriptTypes(operationsSource, {mode: 'transform'}) + '\nreturn TaskBoardOperations;')(canonical, hashJson, IvyError, requireThat, identityHash, mutation, () => '1.0.0', FixedDate);
// Reuse the existing retained-protocol fixture rather than constructing a
// second prepare/apply implementation for this publication-program test.
const support = readFileSync('tests/fixtures/task-board-operations-oracle.mjs', 'utf8');
const memorySource = support.slice(support.indexOf('class Memory {'), support.indexOf('const begin='));
const Memory = new Function('hashJson', 'IvyError', memorySource + '\nreturn Memory;')(hashJson, IvyError);
const engine = readFileSync('services/task-board/src/runtime/engine.ts', 'utf8');
const publicationSource = engine.slice(engine.indexOf('export class Publication'), engine.indexOf('/** Authenticated transitions')).replace('export class', 'class');
const Publication = new Function(stripTypeScriptTypes(publicationSource, {mode: 'transform'}) + '\nreturn Publication;')();
const write = (extra = {}) => ({method: 'write', key: 'task-board/task', value: {title: 'original'}, destination: {objectId: 'destination', expectedRevision: 1}, ...extra});
const create = (extra = {}) => ({method: 'create', key: 'task-board/run', value: {title: 'original'}, label: 'Run', ...extra});
const rows = [], add = (name, steps, callback = true) => rows.push({name, steps, callback});
add('task callback after durable receipt', [write()]); add('non-task does not call task callback', [write({key: 'task-board/run'})]);
add('optional task callback', [write()], false); add('multiple ordered writes', [write(), create(), write({value: {title: 'third'}})]);
add('reset reuses prepared bytes and effect', [write(), {reset: true}, write()]);
add('reset retains original payload bytes', [write(), {reset: true}, write({value: {title: 'replacement'}})]);
add('create scopes name to operation', [create()]); add('create UTF16 label', [create({label: 'Label\ud800'})]);
add('callback failure keeps durable effect', [{callbackFailure: true}, write(), {reset: true}, write()]);
add('unknown effect can be reconciled after reset', [{fault: {unknown: true}}, write(), {reset: true}, write()]);
add('CAS retry does not duplicate callback', [{fault: {conflicts: 1}}, write()]);
add('stage failure consumes program index', [{fault: {conflicts: 12}}, write(), write()]);
add('completed operation refuses additional writes', [{finish: true}, write()]);
add('payload identity mismatch', [write(), {reset: true}, write({destination: {objectId: 'other', expectedRevision: 1}})]);
add('step bound', Array.from({length: 17}, () => write({key: 'task-board/run'})));
for (const row of rows) {
  const store = new Memory(), operations = new Operations(store), events = [], outcomes = []; let fail = false;
  const request = {action: 'edit', operationId: 'epoch:1:id'}, actor = {principalId: 'caller', source: 'worker'};
  const operation = await operations.begin(request, actor);
  const callback = async (pin, value) => { events.push([pin, value]); if (fail) { fail = false; throw new IvyError('callback_failed', 'Fixture callback failed.'); } };
  let publication = new Publication(operations, operation, row.callback ? callback : undefined);
  for (const step of row.steps) {
    if (step.reset) { publication = new Publication(operations, operation, row.callback ? callback : undefined); continue; }
    if (step.fault) { Object.assign(store.fault, step.fault); continue; }
    if (step.callbackFailure) { fail = true; continue; }
    if (step.finish) { await operations.finish(operation.pin.objectId, {operationId: request.operationId, task: null}); continue; }
    try { outcomes.push(await publication[step.method](step.key, structuredClone(step.value), step.method === 'create' ? step.label : step.destination)); }
    catch (error) { outcomes.push({error: {code: error.code, message: error.message, outcome: error.outcome}}); }
  }
  row.expected = {outcomes, events, trace: store.trace, effects: store.effects, index: publication.index, pin: publication.pin};
}
process.stdout.write(JSON.stringify(rows));
