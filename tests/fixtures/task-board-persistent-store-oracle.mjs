// Run only original read methods against a disposable journal and finite RPCs.
import fs from 'node:fs';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {parseArgs} from 'node:util';
import {createHash} from 'node:crypto';
import assert from 'node:assert/strict';
const {values} = parseArgs({options: {'source-root': {type: 'string'}, output: {type: 'string'}, proof: {type: 'string'}, reuse: {type: 'string'}, suite: {type: 'string'}}});
for (const k of ['source-root', 'output', 'proof']) assert.ok(values[k], k);
const root = resolve(values['source-root']), load = p => import(pathToFileURL(resolve(root, p)));
const [{TaskBoardStore}, {IvyError}, {TaskBoardEngine}, {assessSteps}, {readTask}] = await Promise.all([
  load('services/task-board/src/runtime/store.js'), load('packages/contracts/src/errors.js'),
  load('services/task-board/src/runtime/engine.js'), load('services/task-board/src/runtime/next-steps.js'),
  load('services/task-board/src/runtime/task-read.js'),
]);
const schema = await load('services/task-board/src/runtime/schema.js');
const AT = '2026-10-21T05:00:00.000Z', RealDate = Date;
globalThis.Date = class extends RealDate {
  constructor(...args) {super(...(args.length ? args : [AT]));}
  static now() {return RealDate.parse(AT);}
};
const hash = b => 'sha256:' + createHash('sha256').update(b).digest('hex');
const pin = (id = 'self', revision = 7) => ({objectId: id, revision});
const task = extra => ({schemaVersion: 1, taskKey: 'TASK-0001',
  fields: {title: 'Original', description: 'Retained \ud800.', category: null, control: 'agent', priority: 2,
    executionRequirement: null, dueAt: null}, workflowState: 'todo', claim: null, publication: null,
  lastRun: null, latestResult: null, acceptedReview: null, deliveries: [], comments: [], commentDeliveries: [],
  nextSteps: [], consumedSteps: [], runtimeWait: null, completionIssue: null, primaryResourceRef: null,
  updatedAt: AT, ...extra});
const comment = extra => ({commentId: 'c', sequence: 1, authorKind: 'agent', purpose: 'update',
  body: 'Original \udfff.', requests: [], responses: [], attachments: [], ...extra});
const tree = extra => ({kind: 'tree', root: {object: pin('node', 1), length: 1}, length: 1, ...extra});
const node = {schemaVersion: 1, taskId: 'self', kind: 'comment', entry: {comment: comment({}), receipt: null}};
const remote = (key = 'task-board/review', value = {schemaVersion: 1, keep: '\udfff'}, extra = {}) => ({
  object: {id: 'self', contractKey: key, parentId: null, effectivelyArchived: false, name: 'Source', extra: '\ud800'},
  revision: {revision: 7, contractVersion: key === 'task-board/task' ? '2.3.0' : '1.0.0',
    contentHash: 'sha256:retained', mediaType: 'application/json', byteLength: 123, references: {keep: pin('evidence', 3)}},
  content: {encoding: 'json', value}, ...extra,
});
const nodeRecord = {object: {contractKey: 'task-board/comment-node', parentId: 'self', ownerObjectId: 'self'},
  revision: {revision: 1, contractVersion: '1.0.0'}, content: {encoding: 'json', value: node}};
const logRecord = comments => ({object: {contractKey: 'task-board/comments', parentId: 'self', ownerObjectId: null},
  content: {encoding: 'json', value: {taskId: 'self', comments}}});
const read = extra => ({kind: 'read', key: 'task-board/review', address: pin(), primary: true,
  includeArchived: false, retained: false, ...extra});
const row = (id, key, value, extra = {}) => ({id, key, value, parentId: null, name: id,
  revision: 1, createdAt: AT, updatedAt: AT, ...extra});
const cases = [], add = (name, input) => cases.push({name, input});
const one = (record = remote(), steps = [read()], extra = {}) => ({records: {self: record}, steps, ...extra});
add('exact remote document retains metadata and revision', one());
for (const [name, mutate] of [
  ['contract mismatch short circuits absent revision', r => {r.object.contractKey = 'other'; delete r.revision;}],
  ['null revision property failure remains raw', r => {r.revision = null;}],
  ['missing revision property failure remains raw', r => {delete r.revision;}],
  ['unreadable version short circuits null content', r => {r.revision.contractVersion = '9.0.0'; r.content = null;}],
  ['MIME short circuits null content', r => {r.revision.mediaType = 'text/plain'; r.content = null;}],
  ['wrong encoding rejects before scope', r => {r.content.encoding = 'base64'; r.object.parentId = 'other';}],
  ['scope precedes archived and immutable revision', r => {r.object.parentId = 'other'; r.object.effectivelyArchived = true; r.revision.revision = 9;}],
  ['archive precedes immutable revision', r => {r.object.effectivelyArchived = true; r.revision.revision = 9;}],
  ['immutable revision is exact', r => {r.revision.revision = 9;}],
  ['null object property failure remains raw', r => {r.object = null;}],
]) {const r = remote(); mutate(r); add(name, one(r));}
const archived = remote(); archived.object.effectivelyArchived = 'retained truthy';
add('includeArchived uses original truthiness without extra validation', one(archived, [read({includeArchived: true})]));
const foreign = remote(); foreign.object.parentId = 'operation';
add('nonprimary remote payload keeps its original metadata', one(foreign, [read({primary: false})]));
add('latest strings never reuse an action pin cache', one(remote(), [read({address: 'self'}), read({address: 'self'})], {scope: 'action'}));
add('pinned cache returns independent copies of the full document', one(remote(), [read({mutate: true}), read(), read()], {scope: 'action'}));
add('equivalent pin properties ignore order and unrelated fields', one(remote(), [read({address: {revision: 7, objectId: 'self', extra: true}}), read()], {scope: 'action'}));
add('no action has no retained pin cache', one(remote(), [read(), read()]));
add('nested actions share the initiating map', one(remote(), [read(), {kind: 'nested', steps: [read()]}], {scope: 'action'}));
add('separate action maps do not share documents', one(remote(), [{kind: 'action', steps: [read()]}, {kind: 'action', steps: [read()]}]));
const flags = [];
for (const primary of [true, false]) for (const includeArchived of [true, false]) for (const retained of [true, false]) flags.push(read({primary, includeArchived, retained}));
add('all primary archived retained flags participate in the cache key', one(remote(), [...flags, ...flags], {scope: 'action'}));
for (const [name, length, size] of [['exact byte bound caches', 65525, 1], ['one byte above bound is not cached', 65526, 1],
  ['stored byteLength does not replace serialized value length', 3, 999999]]) {
  const r = remote('task-board/review', {text: 'x'.repeat(length)}); r.revision.byteLength = size;
  add(name, one(r, [read(), read()], {scope: 'action'}));
}
add('unsafe canonical number fails only in a pinned action', one(remote('task-board/review', {n: 9007199254740992}), [read(), read()], {scope: 'action'}));
add('latest unsafe number avoids the unused cache encoding', one(remote('task-board/review', {n: 9007199254740992}), [read({address: 'self'})], {scope: 'action'}));
const many = Array.from({length: 257}, (_, i) => read({address: pin('id' + i)}));
add('the action map stops at 256 without evicting the first pin', {autoRecords: true, scope: 'action', steps: [...many, read({address: pin('id0')}), read({address: pin('id256')})]});
add('capacity does not bypass canonical validation', {autoRecords: true, scope: 'action', records: {bad: remote('task-board/review', {n: 9007199254740992})},
  steps: [...many.slice(0, 256), read({address: pin('bad')})]});
add('concurrent reads are not a service lifetime promise cache', one(remote(), [{kind: 'parallel', steps: [read(), read()]}], {scope: 'action', yieldReads: true}));
const header = task({fields: null, commentStore: tree({})});
add('retained task skips both hydration and upgrade', one(remote('task-board/task', header), [read({key: 'task-board/task', retained: true})], {scope: 'action'}));
add('inline remote task uses the current Shared upgrade', one(remote('task-board/task', task({})), [read({key: 'task-board/task'})]));
add('tree hydration and Shared upgrading compose in the actual store', one(remote('task-board/task', task({commentStore: tree({})})), [read({key: 'task-board/task'})], {records: {self: remote('task-board/task', task({commentStore: tree({})})), node: nodeRecord}}));
const legacy = task({commentStore: {objectId: 'log', length: 1, nativeInputs: [{commentId: 'c', delivery: null, handoff: {workflowState: 'waiting'}}]}});
add('retained prefix hydration uses the original operation snapshot', {records: {self: remote('task-board/task', legacy), log: logRecord([comment({nativeInput: {state: 'new'}, delivery: {state: 'new'}}), comment({commentId: 'future', sequence: 2})])}, steps: [read({key: 'task-board/task'})]});
for (const reference of [1, 'raw\ud800', true]) add('raw in operator exception for ' + JSON.stringify(reference), one(remote('task-board/task', task({commentStore: reference})), [read({key: 'task-board/task'})]));
const artifact = {contentHash: 'sha256:a', mediaType: 'application/json'};
for (const mode of ['missing', 'null']) add('actual Store keeps ' + mode + ' evidence RPC parameter', {
  records: {self: remote('task-board/task', task({latestChecks: [{label: 'Evidence.', evidence: [{...artifact, ...(mode === 'null' ? {object: null} : {})}]}]}))},
  optionalRecord: {revision: {...artifact, byteLength: 9}}, steps: [read({key: 'task-board/task'})],
});
add('typed RPC source error remains exact', {records: {}, sourceErrors: {self: {code: 'revision_pruned', message: 'Retained \ud800 source.', outcome: 'unknown', details: {id: '\udfff'}}}, steps: [read()]});
add('arbitrary RPC source object retains identity', {records: {}, rawSource: {id: 'self', value: {raw: '\ud800'}}, steps: [read()]});
const localTask = task({fields: null, commentStore: tree({})});
const preparedRows = (request = {taskId: 'self'}) => [row('operation', 'task-board/operation', {request}), row('local', 'task-board/task', localTask, {parentId: 'operation'})];
add('prepared task uses operation taskId and does not remotely upgrade', {rows: preparedRows(), records: {node: nodeRecord}, steps: [read({key: 'task-board/task', address: 'local', primary: false})]});
add('prepared header skips operation lookup and materialization', {rows: [preparedRows()[1]], records: {}, steps: [read({key: 'task-board/task', address: 'local', primary: false, retained: true})]});
add('prepared primary scope fails before conversation loading', {rows: preparedRows(), records: {node: nodeRecord}, steps: [read({key: 'task-board/task', address: 'local'})]});
add('prepared conversation requires the owning operation task field', {rows: preparedRows({task: 'wrong'}), records: {}, steps: [read({key: 'task-board/task', address: 'local', primary: false})]});
for (const request of [null, false]) add('prepared raw request in failure ' + request, {rows: preparedRows(request), records: {}, steps: [read({key: 'task-board/task', address: 'local', primary: false})]});
add('a falsy prepared reference still requires its original operation', {rows: [row('local', 'task-board/task', task({commentStore: null}))], records: {}, steps: [read({key: 'task-board/task', address: 'local'})]});
add('prepared inline values with no reference skip upgrades and operation lookup', {rows: [row('local', 'task-board/task', {fields: null, untouched: true})], records: {}, steps: [read({key: 'task-board/task', address: 'local'})]});
add('local contract routes to SQL without remote fallback', {rows: [], records: {}, steps: [read({key: 'task-board/operation', address: 'absent'})]});
add('the local immutable revision and its head metadata remain separate', {rows: [row('local', 'task-board/operation', {z: 2, a: '\ud800'}, {revision: 2, raw: '{ "z": 2, "a": "\\ud800" }', older: [{revision: 1, raw: '{"earlier":true}', createdAt: '2026-01-01T00:00:00.000Z'}]})], records: {}, steps: [read({key: 'task-board/operation', address: pin('local', 1)})]});
add('the selected local revision must exist', {rows: [row('local', 'task-board/operation', {request: {}})], records: {}, steps: [read({key: 'task-board/operation', address: pin('local', 9)})]});
add('a different local key does not impersonate the requested remote contract', {rows: [row('self', 'task-board/operation', {request: {}})], ...one()});
add('a blob identity is not a local document revision', {blobs: [{id: 'self', key: 'task-board/review'}], records: {}, steps: [read()]});
add('native SQLite failure does not become a typed Ivy error', {missingTables: true, records: {}, steps: [read()]});
const failed = {phase: 'failed', handoffRun: pin('run', 1), error: {code: 'storage_full', message: 'Source \ud800.'}, operationId: 'op-\udfff'};
add('actual ConditionStore queries retained failed handoffs', {rows: [row('failed', 'task-board/operation', failed)], records: {}, steps: [{kind: 'condition', task: task({completionIssue: {run: pin('run', 1), code: 'other', detail: 'Earlier.'}})}]});
add('actual ConditionStore keeps a missing message raw', {rows: [row('failed', 'task-board/operation', {...failed, error: {code: 'storage_full'}})], records: {}, steps: [{kind: 'condition', task: task({completionIssue: {run: pin('run', 1), detail: 'Earlier.'}})}]});
add('actual header reader drives cached next step assessment', one(remote('task-board/task', task({fields: null})), [{kind: 'assess', task: task({nextSteps: ['a', 'b'].map(id => ({id, scope: 'Header.', requires: [{kind: 'task', taskId: 'self', until: 'idle'}]}))})}], {scope: 'action'}));
add('actual bounded read composes the store condition and tree source', {records: {self: remote('task-board/task', task({commentStore: tree({summary: {directCount: 1, questions: []}})})), node: nodeRecord}, steps: [{kind: 'view', address: pin()}]});
const validComment = comment({author: {serviceNodeId: 'fixture.agent-manager', principalId: 'fixture', generation: 1, source: 'worker'}});
const validNode = {...nodeRecord, content: {encoding: 'json', value: {...node, entry: {comment: validComment, receipt: null}}}};
add('valid remote tree reaches the successful current Shared upgrade', {records: {self: remote('task-board/task', task({commentStore: tree({})})), node: validNode}, steps: [read({key: 'task-board/task'})]});
add('valid remote prefix reaches the successful current Shared upgrade', {records: {self: remote('task-board/task', legacy), log: logRecord([validComment])}, steps: [read({key: 'task-board/task'})]});
add('the actual Store source failure survives shared in flight headers', {records: {}, rawSource: {id: 'self', value: {raw: '\udfff'}}, scope: 'action',
  steps: [{kind: 'assess', task: task({nextSteps: ['a', 'b'].map(id => ({id, scope: 'Raw source.', requires: [{kind: 'task', taskId: 'self', until: 'idle'}]}))})}]});
add('concurrent independent action scopes keep separate pin maps', one(remote(), [{kind: 'parallel', steps: [
  {kind: 'action', steps: [read(), read()]}, {kind: 'action', steps: [read(), read()]},
]}], {yieldReads: true}));
add('the cache limit counts UTF8 bytes of the materialized value', one(remote('task-board/review', {text: 'é'.repeat(32762) + 'x'}), [read(), read()], {scope: 'action'}));
add('an arbitrary primitive source failure stays raw', {records: {}, rawSource: {id: 'self', value: 0}, steps: [read()]});

if (values.suite === 'boundary') {
  cases.length = 0;
  const local = extra => ({rows: [row('local', 'task-board/operation', {retained: true})], records: {},
    steps: [read({key: 'task-board/operation', address: 'local'})], ...extra});
  for (const raw of ['{', '', 'undefined', '\ufeff{}']) add('stored JSON parse source ' + JSON.stringify(raw),
    local({rows: [row('local', 'task-board/operation', {}, {raw})]}));
  add('blob stored JSON reaches the same parser as String Uint8Array', local({sql: ["UPDATE revisions SET value_json=X'7b7d'"]}));
  add('local scope precedes parsing malformed stored JSON', local({root: 'root', rows: [row('local', 'task-board/operation', {}, {raw: '{'})]}));
  add('missing selected revision precedes malformed current JSON', local({rows: [row('local', 'task-board/operation', {}, {raw: '{'})],
    steps: [read({key: 'task-board/operation', address: pin('local', 2)})]}));
  add('invalid UTF8 SQL text is decoded before JSON parsing', local({sql: ["UPDATE revisions SET value_json=CAST(X'22ff22' AS TEXT)"]}));
  add('local head and revision blob fields use original String conversions', local({sql: [
    "UPDATE documents SET name=X'0102',parent_id=X'03',current_revision=X'01',created_at=X'0405',updated_at=X'06'",
    "UPDATE revisions SET created_at=X'0708'"], steps: [read({key: 'task-board/operation', address: 'local', primary: false})]}));
  add('a blob parent does not become a string during the primary scope check', local({root: '3',
    sql: ["UPDATE documents SET parent_id=X'03'", "UPDATE revisions SET value_json='{'"]}));
  add('unsafe SQL integer failure precedes the local scope guard', local({root: 'root',
    sql: ['UPDATE documents SET current_revision=9007199254740992']}));
  add('unsafe selected revision fails before its JSON is parsed', local({
    sql: ["UPDATE revisions SET revision=9007199254740992,value_json='{'"],
    steps: [read({key: 'task-board/operation', address: pin('local', 9007199254740992)})]}));
  for (const address of [null, {revision: 1}, {objectId: false, revision: 1}, {objectId: [], revision: 1},
    {objectId: {}, revision: 1}, {objectId: [1], revision: 1}, {objectId: {z: 1, 2: 1}, revision: 1},
    {objectId: 'local'}, {objectId: 'local', revision: false}, {objectId: 'local', revision: null}]) {
    add('local reference preserves raw property and SQLite binding order ' + JSON.stringify(address), local({
      steps: [read({key: 'task-board/operation', address})]}));
  }
  add('preparing the SQL statement precedes invalid parameter binding', local({missingTables: true,
    steps: [read({key: 'task-board/operation', address: {objectId: false, revision: 1}})]}));
  add('local key projection converts a blob without inventing a local contract', {
    rows: [row('self', 'task-board/operation', {})], sql: ["UPDATE documents SET key=X'01'"], ...one()});
  const prefix = (comments, reference, extra = {}) => ({rows: [row('operation', 'task-board/operation', {request: {taskId: 'self'}}),
    row('local', 'task-board/task', {fields: null, commentStore: reference}, {parentId: 'operation'})],
    records: {log: logRecord(comments)}, steps: [read({key: 'task-board/task', address: 'local', primary: false})], ...extra});
  const reference = extra => ({objectId: 'log', length: 1, nativeInputs: [], ...extra});
  add('null retained prefix comment keeps its destructuring failure', prefix([null], reference({})));
  add('retained prefix primitive comments keep object spread semantics', prefix(['A\ud800', 42], reference({length: 2})));
  for (const nativeInputs of [null, {}, [null], [1], [{commentId: 'c', body: 'first'}, {commentId: 'c', body: 'last', unknown: '\udfff'}]]) {
    add('raw retained prefix native snapshots ' + JSON.stringify(nativeInputs), prefix([comment({})], reference({nativeInputs})));
  }
  add('retained prefix missing native snapshots remains a raw property error', prefix([comment({})], {objectId: 'log', length: 1}));
  add('an invalid prefix length fails before missing snapshot access', prefix([comment({})], {objectId: 'log', length: 2}));
  add('retained log source failure keeps its exact identity', prefix([], reference({length: 0}), {
    rawSource: {id: 'log', value: {raw: '\ud800'}}}));
  const review = acceptance => ({schemaVersion: 1, taskId: 'task', result: pin('result', 1), acceptance,
    createdAt: AT, actor: {principalId: 'fixture', serviceNodeId: 'fixture.agent-manager', generation: 1, source: 'worker'}, operationId: 'operation'});
  const write = (extra = {}) => ({kind: 'write', key: 'task-board/review', value: review('Accepted.'), mutation: 'retained-mutation',
    destination: {objectId: 'self', expectedRevision: 7}, ...extra});
  const written = (id = 'self', revision = 8, byteLength = 123) => ({object: {id, parentId: null}, revision: {revision, byteLength}});
  const mixed = (steps, extra = {}) => ({autoRecords: true, scope: 'action', steps, writeResults: [written()], ...extra});
  add('successful writes invalidate every retained pin variant', mixed([...flags, write(), ...flags]));
  add('failed writes preserve previously read pins', mixed([read(), write(), read()], {
    writeResults: [{error: {code: 'storage_full', message: 'Retained write.', outcome: 'not_executed'}}]}));
  add('another object write preserves unrelated cached pins', mixed([read(), write({destination: {objectId: 'other', expectedRevision: 7}}), read(), read({address: pin('other', 8)})],
    {writeResults: [written('other')]}));
  add('write seeds consume a slot but never satisfy a pinned read', mixed([write(), read({address: pin('self', 8)}), read({address: pin('self', 8)})]));
  const capacity = [write(), ...many.slice(0, 256), read({address: pin('id255')})];
  add('small canonical writes seed even when reported byteLength is large', mixed(capacity, {writeResults: [written('self', 8, 999999)]}));
  add('large canonical writes do not seed when reported byteLength is small', mixed([
    write({value: review('é'.repeat(32768))}), ...many.slice(0, 256), read({address: pin('id255')})], {writeResults: [written('self', 8, 1)]}));
  add('repeated writes replace the same unmatched seed', mixed([write(), write({destination: {objectId: 'self', expectedRevision: 8}}),
    ...many.slice(0, 255), read({address: pin('id254')})], {writeResults: [written(), written('self', 9)]}));
}

const pins = ['services/task-board/src/runtime/store.js', 'services/task-board/src/runtime/comment-store.js',
  'services/task-board/src/runtime/schema.js', 'services/task-board/src/runtime/task-read.js', 'services/task-board/src/runtime/engine.js',
  'services/task-board/src/runtime/next-steps.js', 'packages/sdk/src/task-comments.js', 'packages/contracts/src/task-board-records.js',
  'packages/contracts/src/task-board-questions.js'].map(path => {const bytes = fs.readFileSync(resolve(root, path)); return {path, contentHash: hash(bytes), byteLength: bytes.length};});
const reused = values.reuse ? JSON.parse(fs.readFileSync(values.reuse)) : null;
if (reused) assert.deepEqual(reused.pins, pins);
const results = []; let executed = 0, reusedCount = 0;
for (const entry of cases) {
  const prior = reused?.cases.find(c => c.name === entry.name);
  if (prior) {assert.deepEqual(prior.input, entry.input); results.push(prior); reusedCount++; continue;}
  executed++;
  const input = structuredClone(entry.input), before = JSON.stringify(input), trace = [], raw = structuredClone(input.rawSource?.value);
  const parseErrors = []; let parsedFailure, writes = 0;
  const client = {async request(method, params) {
    const record = {method}; if (params !== undefined) record.params = structuredClone(params); trace.push(record);
    if (method === 'objects.write') {
      const result = input.writeResults[writes++]; assert.ok(result, 'Unexpected write');
      if (result.error) throw new IvyError(result.error.code, result.error.message, result.error.outcome, result.error.details);
      return structuredClone(result);
    }
    assert.equal(method, 'objects.read');
    if (input.yieldReads) await Promise.resolve();
    const id = params?.objectId;
    if (id === input.rawSource?.id && input.rawSource) throw raw;
    const error = input.sourceErrors?.[id]; if (error) throw new IvyError(error.code, error.message, error.outcome, error.details);
    if (params == null && Object.hasOwn(input, 'optionalRecord')) return structuredClone(input.optionalRecord);
    if (Object.hasOwn(input.records ?? {}, id)) return structuredClone(input.records[id]);
    assert.ok(input.autoRecords, 'Unexpected RPC: ' + JSON.stringify(params));
    const result = remote(); result.object.id = id; result.revision.revision = params.revision; return result;
  }};
  const store = new TaskBoardStore(client, input.root ?? null);
  const parse = JSON.parse;
  try {
    for (const r of input.rows ?? []) {
      store.local.prepare('INSERT INTO documents VALUES(?,?,?,?,?,?,?)').run(r.id, r.key, r.name, r.parentId, r.revision, r.createdAt, r.updatedAt);
      store.local.prepare('INSERT INTO revisions VALUES(?,?,?,?)').run(r.id, r.revision, r.raw ?? JSON.stringify(r.value), r.createdAt);
      for (const old of r.older ?? []) store.local.prepare('INSERT INTO revisions VALUES(?,?,?,?)').run(r.id, old.revision, old.raw, old.createdAt);
    }
    for (const r of input.blobs ?? []) store.local.prepare('INSERT INTO blobs VALUES(?,?,?,?,?)').run(r.id, r.key, r.id, null, Buffer.alloc(0));
    for (const sql of input.sql ?? []) store.local.exec(sql);
    if (input.missingTables) store.local.exec('DROP TABLE revisions; DROP TABLE documents;');
    // Observe parser provenance without replacing the original parser or catch.
    JSON.parse = function(text, ...args) {
      try {return parse(text, ...args);} catch (error) {
        parsedFailure = error; parseErrors.push({text, kind: error.name, message: error.message}); throw error;
      }
    };
    async function steps(items) {
      const observations = [];
      for (const item of items) {
        let result;
        try {
          if (item.kind === 'action' || item.kind === 'nested') result = await store.withReadCache(() => steps(item.steps));
          else if (item.kind === 'parallel') result = await Promise.all(item.steps.map(s => steps([s]).then(r => r[0])));
          else if (item.kind === 'write') result = await store.writePrepared(item.key, item.value, item.mutation, item.destination);
          else if (item.kind === 'condition') result = TaskBoardEngine.prototype.condition.call({store}, item.task);
          else if (item.kind === 'assess') result = await assessSteps(store, item.task, AT);
          else if (item.kind === 'view') {
            const header = await store.read('task-board/task', item.address, true, true, true);
            result = await readTask(store, header, {}, (task, condition) => TaskBoardEngine.prototype.condition.call({store}, task, condition));
          } else result = await store.read(item.key, item.address, item.primary, item.includeArchived, item.retained);
          observations.push({value: structuredClone(result)});
          if (item.mutate) {result.value = {changed: true}; result.metadata.extra = 'changed'; result.revision.contentHash = 'changed';}
        } catch (error) {
          if (parsedFailure !== undefined && error === parsedFailure) observations.push({parserRejected: true, identity: true});
          else if (input.rawSource && error === raw) observations.push({source: structuredClone(raw), identity: true});
          else if (error instanceof IvyError) observations.push({ivy: error.toWire()});
          else {assert.notEqual(error.name, 'ReferenceError'); observations.push({javascript: {kind: error.name, message: error.message}});}
        }
      }
      return observations;
    }
    const expected = input.scope === 'action' ? await store.withReadCache(() => steps(input.steps)) : await steps(input.steps);
    assert.equal(JSON.stringify(input), before);
    results.push({...entry, expected, trace, ...(parseErrors.length ? {parseErrors} : {})});
  } finally {JSON.parse = parse; store.close();}
}
const keys = [...new Set(cases.flatMap(c => [
  ...(c.input.rows ?? []).map(r => r.key),
  ...Object.values(c.input.records ?? {}).map(r => r.object?.contractKey).filter(k => k && k !== 'task-board/comment-node'),
  ...c.input.steps.map(s => s.key).filter(Boolean), 'task-board/task', 'task-board/operation',
]))].filter(k => k !== 'other' && k !== 'task-board/comments');
const contracts = Object.fromEntries(keys.map(key => [key, {version: schema.contractVersion(key), readable: schema.readableVersions(key), local: schema.localContractKeys.has(key)}]));
fs.writeFileSync(values.output, JSON.stringify({format: 'task-board-current-persistent-store-original-v1', at: AT, pins, contracts, cases: results}, null, 2) + '\n');
fs.writeFileSync(values.proof, JSON.stringify({at: new RealDate().toISOString(), node: process.version, pins, executed, reused: reusedCount,
  fixture: {byteLength: fs.statSync(values.output).size, contentHash: hash(fs.readFileSync(values.output))},
  scope: values.suite === 'boundary'
    ? 'Actual Store local reads, raw retained prefixes and prepared remote write/cache methods with isolated in-memory SQLite and finite mock RPCs. Parser diagnostics are retained separately; only rejection and cause identity are compared across parsers. No live writes, service constructor, socket, publication/recovery, deployment or SDK catch policy is exercised.'
    : 'Actual Store/TaskComments/read/condition/next-step methods with isolated in-memory SQLite and finite read-only RPCs. No service constructor, socket, live Hive, write/publication/recovery, deployment or global SDK wire policy is exercised.'}, null, 2) + '\n');
console.log(JSON.stringify({cases: results.length, executed, reused: reusedCount, originalRpcCalls: results.reduce((n, c) => n + c.trace.length, 0)}));
