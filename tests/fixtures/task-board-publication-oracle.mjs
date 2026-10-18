import {registerHooks, stripTypeScriptTypes} from 'node:module';
import {existsSync, readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(s, c, next) {
  if (s.startsWith('.') && s.endsWith('.js') && c.parentURL) {
    const u = new URL(s.slice(0, -3) + '.ts', c.parentURL);
    if (existsSync(fileURLToPath(u))) return next(u.href, c);
  }
  return next(s, c);
}});
const {IvyError, requireThat} = await import('../../packages/contracts/src/errors.ts');
const {digest} = await import('../../packages/contracts/src/canonical.ts');
const source = readFileSync('services/task-board/src/runtime/store.ts', 'utf8');
const methods = source.slice(source.indexOf('  async writeAttachment('), source.indexOf('  saveLocalBlob('));
const results = [], add = (name, input) => results.push({name, input});
const attachment = (length, extra = {}) => ({kind: 'attachment', length, taskId: 'task', operationId: 'operation', attachmentId: 'attachment', ...extra});
for (const length of [0, 1, 2, 3, 4, 31, 256, 4097, 8 * 1024 * 1024, 8 * 1024 * 1024 + 1]) add('attachment bytes ' + length, attachment(length));
add('attachment UTF16 identities', attachment(5, {taskId: 'task\ud800', operationId: 'operation\udfff', attachmentId: 'attachment\ud800'}));
add('attachment repeated retained scope', attachment(12, {repeat: 2}));
for (const [code, outcome] of [['revision_conflict', 'not_executed'], ['mutation_conflict', 'not_executed'], ['outcome_unknown', 'unknown'], ['service_unavailable', 'unknown']]) {
  add('attachment original error ' + code, attachment(9, {error: {code, outcome}}));
}
const page = (options = {}, extra = {}) => ({kind: 'page', root: 'root', key: 'task-board/task', versions: ['1.0.0', '1.1.0'], options, ...extra});
add('page defaults', page()); add('page null root', page({}, {root: null}));
add('page nullish defaults', page({contractVersions: null, orderBy: null, limit: null, where: null, cursor: null, select: null}));
add('page explicit options', page({contractVersions: ['1.1.0'], orderBy: [{field: 'data:/fields/priority', direction: 'desc'}], limit: 7, select: ['data:/fields/title'], cursor: 'opaque-cursor'}));
add('page empty arrays remain explicit', page({contractVersions: [], orderBy: [], select: []}));
add('page empty cursor omitted', page({cursor: ''}));
add('page UTF16 cursor', page({cursor: 'cursor\ud800'}));
add('page zero limit remains explicit', page({limit: 0}));
add('page predicate is grouped under root', page({where: {op: 'or', args: [{op: 'eq', field: 'object.parentId', value: 'other'}, {op: 'eq', field: 'data:/fields/title', value: 'scope\udfff'}]}}));
add('page null root predicate', page({where: {op: 'not', arg: {op: 'isNull', field: 'data:/claim'}}}, {root: null}));
add('page reply preserved', page({}, {reply: {items: [{objectId: 'record', revision: 8, values: {'data:/fields/title': 'Exact'}}], nextCursor: 'next'}}));
for (const [code, outcome] of [['service_unavailable', 'not_executed'], ['deadline_exceeded', 'unknown']]) add('page original error ' + code, page({}, {error: {code, outcome}}));
for (const row of results) {
  const i = row.input, trace = [], scopes = [];
  const scopedOperationId = async (_client, scope) => { scopes.push(structuredClone(scope)); return 'retained-identity'; };
  const Store = new Function('requireThat', 'scopedOperationId', 'version', 'pinOf', 'localContractKeys', 'readableVersions', stripTypeScriptTypes('class Store {client;rootObjectId;\n' + methods + '\n}') + '\nreturn Store;')(
    requireThat, scopedOperationId, '1.0.0', r => ({objectId: r.object.id, revision: r.revision.revision}), new Set(), () => i.versions,
  );
  const store = new Store(); store.rootObjectId = i.root ?? null;
  store.client = {request: async (method, params) => {
    if (params.content) {
      const base64 = params.content.value;
      params.content.value = {base64Length: base64.length, base64Hash: digest(Buffer.from(base64, 'utf8'))};
    }
    trace.push([method, params]);
    if (i.error) throw new IvyError(i.error.code, 'Fixture publication failure.', i.error.outcome);
    return i.kind === 'attachment' ? {object: {id: 'binary'}, revision: {revision: 1}} : i.reply ?? {items: [], nextCursor: null};
  }};
  const outcomes = [];
  for (let n = 0; n < (i.repeat ?? 1); n++) {
    try {
      const bytes = i.kind === 'attachment' ? Buffer.from(Uint8Array.from({length: i.length}, (_, i) => (i * 37 + 11) & 255)) : null;
      outcomes.push(i.kind === 'attachment' ? await store.writeAttachment(i.taskId, i.operationId, i.attachmentId, bytes) : await store.page(i.key, i.options));
    } catch (error) { outcomes.push({error: {code: error.code, message: error.message, outcome: error.outcome}}); }
  }
  row.expected = {outcomes, trace, scopes};
}
process.stdout.write(JSON.stringify(results));
