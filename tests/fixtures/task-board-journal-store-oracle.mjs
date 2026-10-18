import {existsSync, readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
import {registerHooks, stripTypeScriptTypes} from 'node:module';
import {AsyncLocalStorage} from 'node:async_hooks';
import {DatabaseSync} from 'node:sqlite';
registerHooks({resolve(s, c, next) {
  if (s.startsWith('.') && s.endsWith('.js') && c.parentURL) {
    const url = new URL(s.slice(0, -3) + '.ts', c.parentURL);
    if (existsSync(fileURLToPath(url))) return next(url.href, c);
  }
  return next(s, c);
}});
const {canonical, hashJson, digest} = await import('../../packages/contracts/src/canonical.ts');
const {IvyError, requireThat} = await import('../../packages/contracts/src/errors.ts');
const source = readFileSync('services/task-board/src/runtime/store.ts', 'utf8');
const schema = source.match(/\.exec\(`(PRAGMA journal_mode=WAL;[\s\S]*?)`\);/)[1];
const archive = readFileSync('packages/sdk/src/receipt-archive.ts', 'utf8');
const hasUnresolvedOutcome = new Function(stripTypeScriptTypes(archive.slice(archive.indexOf('export function hasUnresolvedOutcome'), archive.indexOf('/** Receipts share')).replace('export ', '')) + '\nreturn hasUnresolvedOutcome;')();
const helpers = source.slice(source.indexOf('export const taskAttachmentPins'), source.indexOf('// The journal may run'));
const taskAttachmentPins = new Function('hashJson', stripTypeScriptTypes(helpers.replace('export ', '')) + '\nreturn taskAttachmentPins;')(hashJson);
const methods = source.slice(source.indexOf('  private async retainedLocalRoot('), source.indexOf('  async allocateTaskKey(')) +
  source.slice(source.indexOf('  private localDocument<'), source.indexOf('  close(): void'));
const time = '2026-01-02T03:04:05.000Z';
class FixedDate extends Date { constructor(value = time) { super(value); } static now() { return Date.parse(time); } }
const localKeys = ['task-board/operation', 'task-board/native-plan', 'task-board/coordination', 'task-board/native-read-evidence', 'task-board/native-evidence'];
const Store = new Function('canonical', 'hashJson', 'digest', 'IvyError', 'requireThat', 'hasUnresolvedOutcome', 'contractVersion', 'readableVersions', 'upgradeRecord', 'localContractKeys', 'taskAttachmentPins', 'randomUUID', 'Date', 'pinOf',
  stripTypeScriptTypes(`class Store { local; client; rootObjectId; reads; historyLimits; constructor(db,client,root,reads,limits){this.local=db;this.client=client;this.rootObjectId=root;this.reads=reads;this.historyLimits=limits;}\n${methods}\n}`) + '\nreturn Store;')(
  canonical, hashJson, digest, IvyError, requireThat, hasUnresolvedOutcome, () => '1.0.0', () => ['1.0.0'], (_key, value) => value,
  new Set(localKeys), taskAttachmentPins, () => 'new-document', FixedDate, result => ({objectId: result.object.id, revision: result.revision.revision}));
const pin = (id = 'remote', revision = 1) => ({objectId: id, revision});
const create = (name = 'record', parentId = 'root') => ({create: {parentId, name}});
const value = {schemaVersion: 1, value: 'original'}, changed = {...value, value: 'changed'};
const doc = (id = 'local', key = 'task-board/native-plan', parent = 'root', data = value) => ({id, key, parent, value: data});
const read = (address = pin(), extra = {}) => ({method: 'read', key: 'task-board/run', address, ...extra});
const named = (extra = {}) => ({method: 'named', key: 'task-board/run', name: 'record', ...extra});
const write = (extra = {}) => ({method: 'writePrepared', key: 'task-board/run', value, destination: {objectId: 'remote', expectedRevision: 1}, ...extra});
const rows = [], add = (name, input = {}) => rows.push({name, input: {localKeys, root: 'root', steps: [read()], documents: [], ...input}});
add('exact immutable read');
add('latest reads are fresh', {cache: true, steps: [read('remote'), read('remote')]});
add('immutable cache clone', {cache: true, steps: [read(pin(), {mutate: true}), read()]});
add('cache scope and archived flag', {cache: true, steps: [read(), read(pin(), {primary: false}), read(pin(), {includeArchived: true}), read()]});
add('large reads are not cached', {cache: true, byteLength: 65537, steps: [read(), read()]});
add('cache byte boundary', {cache: true, byteLength: 65536, steps: [read(), read()]});
add('write invalidates pins', {cache: true, steps: [read(), write(), read()]});
add('separate cache after action', {cache: true, steps: [read(), {clearCache: true}, read()]});
add('concurrent cache fills retain the distinct pin bound', {cache: true, dynamicId: true, steps: [
  {parallel: [read(pin('id0')), read(pin('id0'))]},
  ...Array.from({length: 255}, (_, index) => read(pin('id' + (index + 1)))), read(pin('id255')), read(pin('id0')),
]});
for (const [field, selected] of [['contractKey', 'other'], ['contractVersion', '9.0.0'], ['mediaType', 'text/plain'], ['encoding', 'base64']]) add('read refuses ' + field, {[field]: selected});
add('primary root mismatch', {parent: 'other'});
add('prepared payload may use another parent', {parent: 'other', steps: [read(pin(), {primary: false})]});
add('archived refusal', {archived: true});
add('archived read admitted explicitly', {archived: true, steps: [read(pin(), {includeArchived: true})]});
add('pin revision mismatch', {revision: 2});
add('remote error is retained', {readError: {code: 'service_unavailable', outcome: 'unknown'}});
add('named missing', {query: {items: [], nextCursor: null}, steps: [named()]});
add('named identity', {steps: [named()]});
add('named identity not filtered by version', {contractVersion: '9.0.0', steps: [named()]});
add('named duplicate', {query: {items: [pin(), pin('other')], nextCursor: null}, steps: [named()]});
add('named cursor', {query: {items: [pin()], nextCursor: 'more'}, steps: [named()]});
add('named explicit null parent', {root: 'root', parent: null, steps: [named({parent: null})]});
add('local read', {documents: [doc()], steps: [read('local', {key: 'task-board/native-plan'})]});
add('local pin is never remotely cached', {cache: true, documents: [doc()], steps: [read(pin('local'), {key: 'task-board/native-plan'}), read(pin('local'), {key: 'task-board/native-plan'})]});
add('local payload read by physical key', {documents: [doc('local', 'task-board/run', 'operation')], steps: [read(pin('local'), {primary: false})]});
add('local scope refusal', {documents: [doc('local', 'task-board/run', 'operation')], steps: [read('local')]});
add('missing local record does not fall through', {steps: [read('missing', {key: 'task-board/native-plan'})]});
add('local named', {documents: [doc()], steps: [named({key: 'task-board/native-plan', name: 'local'})]});
add('local prepared name', {documents: [doc('operation', 'task-board/operation'), doc('payload', 'task-board/run', 'operation')], steps: [named({name: 'payload', parent: 'operation'})]});
add('local create replay', {steps: [write({method: 'write', key: 'task-board/native-plan', destination: create()}), write({method: 'write', key: 'task-board/native-plan', destination: create()})]});
add('local create identity conflict', {documents: [doc('local')], steps: [write({method: 'write', key: 'task-board/native-plan', value: changed, destination: create('local')})]});
add('local update and stale CAS', {documents: [doc()], steps: [write({method: 'write', key: 'task-board/native-plan', value: changed, destination: {objectId: 'local', expectedRevision: 1}}), write({method: 'write', key: 'task-board/native-plan', destination: {objectId: 'local', expectedRevision: 1}})]});
add('local prepared payload stays local', {documents: [doc('operation', 'task-board/operation')], steps: [write({destination: create('payload', 'operation')})]});
add('local update cannot cross the remote route', {documents: [doc('local', 'task-board/run', 'operation')], steps: [write({destination: {objectId: 'local', expectedRevision: 1}})]});
add('retention refuses local admission', {limitRows: 0, steps: [write({method: 'write', key: 'task-board/native-plan', destination: create()})]});
add('retained write identity', {steps: [write()]});
add('remote create shape', {steps: [write({destination: create()})]});
add('create conflict same bytes', {writeError: {code: 'revision_conflict'}, steps: [write({destination: create()})]});
add('create conflict different bytes', {remoteValue: changed, writeError: {code: 'revision_conflict'}, steps: [write({destination: create()})]});
add('create conflict later revision', {revision: 2, writeError: {code: 'revision_conflict'}, steps: [write({destination: create()})]});
add('create conflict missing winner', {query: {items: [], nextCursor: null}, writeError: {code: 'revision_conflict'}, steps: [write({destination: create()})]});
add('mutation conflict is not create recovery', {writeError: {code: 'mutation_conflict'}, steps: [write({destination: create()})]});
add('unknown write is not replayed', {writeError: {code: 'outcome_unknown', outcome: 'unknown'}, steps: [write({destination: create()})]});
const attachment = (id, revision = 1) => ({object: pin(id, revision)});
const task = {latestResult: pin('result'), lastRun: pin('run'), attachments: [attachment('a'), attachment('b')], comments: [{attachments: [attachment('a'), attachment('a', 2), attachment('c')]}]};
add('task exact references and dedup', {steps: [write({key: 'task-board/task', value: task})]});
add('task reference boundary', {steps: [write({key: 'task-board/task', value: {...task, attachments: Array.from({length: 254}, (_, i) => attachment('a' + i)), comments: []}})]});
add('task reference overflow', {steps: [write({key: 'task-board/task', value: {...task, attachments: Array.from({length: 255}, (_, i) => attachment('a' + i)), comments: []}})]});
add('delivery source reference', {steps: [write({key: 'task-board/delivery', value: {chatRequest: {source: pin('source')}}})]});
add('result excludes local artifacts', {documents: [doc()], steps: [write({key: 'task-board/result', value: {content: {artifacts: [attachment('a'), attachment('local')], checks: [{evidence: [attachment('b')]}]}}})]});
add('result reference cap', {steps: [write({key: 'task-board/result', value: {content: {artifacts: Array.from({length: 257}, (_, i) => attachment('a' + i)), checks: []}}})]});
const tables = ['documents', 'revisions', 'blobs', 'markers', 'cleanup_roots', 'cleanup_dependencies', 'active_gates', 'operation_gates', 'workspace_reservations', 'storage_usage', 'retired_operations'];
for (const row of rows) {
  const i = row.input, db = new DatabaseSync(':memory:'); db.exec(schema);
  for (const d of i.documents) {
    db.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(d.id, d.key, d.id, d.parent, 1, time, time);
    db.prepare('INSERT INTO revisions VALUES(?,?,?,?)').run(d.id, 1, canonical(d.value), time);
  }
  const readResult = {object: {id: 'remote', contractKey: i.contractKey ?? 'task-board/run', parentId: 'parent' in i ? i.parent : i.root, effectivelyArchived: i.archived ?? false},
    revision: {revision: i.revision ?? 1, contractVersion: i.contractVersion ?? '1.0.0', mediaType: i.mediaType ?? 'application/json', contentHash: hashJson(i.remoteValue ?? value), byteLength: i.byteLength ?? 100},
    content: {encoding: i.encoding ?? 'json', value: i.remoteValue ?? value}};
  const writeResult = {object: {id: 'remote', parentId: i.root}, revision: {revision: 2, byteLength: 100}};
  const trace = [], request = async (method, params) => {
    trace.push([method, structuredClone(params)]);
    const error = method === 'objects.read' ? i.readError : method === 'objects.write' ? i.writeError : null;
    if (error) throw new IvyError(error.code, 'Fixture RPC failure.', error.outcome ?? 'not_executed');
    const result = structuredClone(method === 'objects.read' ? readResult : method === 'objects.query' ? i.query ?? {items: [pin()], nextCursor: null} : writeResult);
    if (method === 'objects.read' && i.dynamicId) result.object.id = params.objectId;
    return result;
  };
  const reads = new AsyncLocalStorage(), store = new Store(db, {request}, i.root, reads, {count: i.limitRows ?? 10000, bytes: 268435456}), outcomes = [];
  await reads.run(i.cache ? new Map() : undefined, async () => {
    for (const step of i.steps) {
      if (step.clearCache) { reads.getStore()?.clear(); continue; }
      if (step.parallel) {
        const selected = await Promise.all(step.parallel.map(step => store.read(step.key, step.address)));
        outcomes.push(selected.map(d => ({pin: d.pin, value: d.value, parentId: d.metadata.parentId, contentHash: d.revision.contentHash})));
        continue;
      }
      try {
        const selected = step.method === 'read' ? await store.read(step.key, step.address, step.primary ?? true, step.includeArchived ?? false) : step.method === 'named' ? await store.named(step.key, step.name, 'parent' in step ? step.parent : i.root) : await store[step.method](step.key, step.value, 'retained-identity', step.destination);
        outcomes.push(structuredClone(selected?.pin && selected?.value ? {pin: selected.pin, value: selected.value, parentId: selected.metadata.parentId, contentHash: selected.revision.contentHash} : selected));
        if (step.mutate && selected?.value) selected.value.value = 'caller mutation';
      } catch (error) { outcomes.push({error: {code: error.code, message: error.message, outcome: error.outcome}}); }
    }
  });
  i.readResult = readResult; i.writeResult = writeResult;
  i.allowed = [...new Set([...i.documents.map(d => hashJson(d.value)), hashJson(readResult.content.value), ...i.steps.filter(s => s.value).map(s => hashJson(s.value))])];
  row.expected = {outcomes, trace, tables: Object.fromEntries(tables.map(table => [table, db.prepare('SELECT * FROM ' + table + ' ORDER BY rowid').all()]))};
  db.close();
}
process.stdout.write(JSON.stringify(rows));
