// Capture only the selected pure read/catch seams of the pinned release.
import fs from 'node:fs';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
import {parseArgs} from 'node:util';
import assert from 'node:assert/strict';
const {values} = parseArgs({options: {'source-root': {type: 'string'}, output: {type: 'string'}, proof: {type: 'string'}, reuse: {type: 'string'}}});
for (const k of ['source-root', 'output', 'proof']) assert.ok(values[k], k);
const root = resolve(values['source-root']), load = p => import(pathToFileURL(resolve(root, p)));
const [{assessSteps, validateNextSteps}, {readTask}, {TaskComments}, {TaskBoardStore}, {TaskBoardEngine}, {IvyError}] = await Promise.all([
  load('services/task-board/src/runtime/next-steps.js'), load('services/task-board/src/runtime/task-read.js'),
  load('services/task-board/src/runtime/comment-store.js'), load('services/task-board/src/runtime/store.js'),
  load('services/task-board/src/runtime/engine.js'), load('packages/contracts/src/errors.js'),
]);
const hash = b => 'sha256:' + createHash('sha256').update(b).digest('hex');
const AT = '2026-10-20T12:00:00.000Z', RealDate = Date;
let clocks = 0;
globalThis.Date = class extends RealDate {
  constructor(...args) { if (!args.length) clocks++; super(...(args.length ? args : [AT])); }
  static now() { return RealDate.parse(AT); }
};
const task = patch => ({schemaVersion: 1, taskKey: 'TASK-0001', fields: {title: 'Fixture', category: null,
  control: 'agent', priority: 2, executionRequirement: null, dueAt: null}, workflowState: 'todo',
  claim: null, publication: null, lastRun: null, latestResult: null, acceptedReview: null, deliveries: [],
  comments: [], commentDeliveries: [], nextSteps: [], consumedSteps: [], runtimeWait: null,
  completionIssue: null, primaryResourceRef: null, updatedAt: AT, ...patch});
const step = (id, other) => ({id, scope: 'Original source identity.', requires: [{kind: 'task', taskId: other, until: 'idle'}]});
const tree = {kind: 'tree', root: {object: {objectId: 'node', revision: 1}, length: 1}, length: 1,
  summary: {directCount: 1, questions: []}};
const comment = {commentId: 'c', sequence: 1, authorKind: 'agent', purpose: 'update', body: 'Body \ud800', requests: [], responses: [], attachments: []};
const node = {schemaVersion: 1, taskId: 'self', kind: 'comment', entry: {comment, receipt: null}};
const header = value => ({pin: {objectId: 'self', revision: 7}, metadata: {effectivelyArchived: false}, value});
const cases = [];
const add = (kind, name, input) => cases.push({kind, name, input});
add('assess', 'shared in-flight header keeps opaque object identity', {task: task({nextSteps: [step('a', 'bad'), step('b', 'bad')]}), failRead: 'bad', throwValue: {raw: '\udfff'}});
add('assess', 'available null throw beats stalled independent header', {task: task({nextSteps: [step('a', 'bad'), step('b', 'stalled')]}), failRead: 'bad', stalled: 'stalled', throwValue: null});
add('validate', 'full validation read retains arbitrary primitive throw', {task: task({}), steps: [step('a', 'bad')], failRead: 'bad', throwValue: '\ud800'});
add('read', 'full read failure reaches the original caller unchanged', {header: header(task({commentStore: {...tree, summary: undefined}})), failRead: 'self', throwValue: {raw: 'full'}});
add('read', 'actual tree range retains primitive source identity', {header: header(task({commentStore: tree})), failNode: true, throwValue: 0});
add('read', 'eager condition source failure precedes tree range', {header: header(task({commentStore: tree,
  completionIssue: {code: 'other', detail: 'Retained.', run: {objectId: 'run', revision: 1}}})), failCondition: true, throwValue: {raw: 'condition'}});
const invalid = task({commentStore: {...tree, summary: undefined}}); delete invalid.nextSteps;
add('read', 'raw summary key error precedes full fallback', {header: header(invalid)});
add('read', 'indexed read uses the real tree source', {header: header(task({commentStore: tree}))});
add('read', 'full fallback hydrates before actual Shared upgrade', {header: header(task({commentStore: {...tree, summary: undefined},
  latestChecks: [{label: 'Original.', evidence: [{object: {objectId: 'artifact', revision: 2}, contentHash: 'sha256:a', mediaType: 'application/json'}]}]}))});
add('store', 'retained value skips hydration and upgrade', {header: header(task({commentStore: tree, fields: null})), retained: true});
add('store', 'prepared value hydrates with operation taskId and skips upgrade', {header: header(task({commentStore: {...tree, summary: undefined}, fields: null})),
  prepared: true, operation: {request: {taskId: 'self'}}});
add('store', 'remote tree then evidence then raw Shared failure', {header: header(task({commentStore: {...tree, summary: undefined}, fields: null,
  latestChecks: [{label: 'Original.', evidence: [{object: {objectId: 'artifact', revision: 2}, contentHash: 'sha256:a', mediaType: 'application/json'}]}]}))});
add('read', 'actual tree RPC Ivy error stays typed', {header: header(task({commentStore: tree})), failNode: true,
  throwIvy: {code: 'revision_pruned', message: 'Exact \ud800 source.', outcome: 'unknown', details: {raw: '\udfff'}}});

const results = [];
const reused = values.reuse ? JSON.parse(fs.readFileSync(values.reuse)) : null;
let executed = 0, reusedCount = 0;
for (const entry of cases) {
  const prior = reused?.cases.find(c => c.name === entry.name);
  if (prior) {assert.equal(JSON.stringify(prior.input), JSON.stringify(entry.input)); results.push(prior); reusedCount++; continue;}
  executed++;
  const input = structuredClone(entry.input), before = JSON.stringify(input), trace = [], raw = structuredClone(input.throwValue);
  clocks = 0;
  const client = {async request(method, params) {
    trace.push({method, params: structuredClone(params)});
    if (params?.objectId === 'node') {
      if (input.failNode) {
        if (input.throwIvy) throw new IvyError(input.throwIvy.code, input.throwIvy.message, input.throwIvy.outcome, input.throwIvy.details);
        throw raw;
      }
      return {object: {contractKey: 'task-board/comment-node', parentId: 'self', ownerObjectId: 'self'},
        revision: {contractVersion: '1.0.0', revision: 1}, content: {encoding: 'json', value: structuredClone(node)}};
    }
    if (params?.objectId === 'artifact') return {revision: {contentHash: 'sha256:a', mediaType: 'application/json', byteLength: 9}};
    assert.equal(params.objectId, 'self');
    return {object: {id: 'self', contractKey: 'task-board/task', parentId: null, effectivelyArchived: false},
      revision: {contractVersion: '2.3.0', revision: 7, mediaType: 'application/json'}, content: {encoding: 'json', value: structuredClone(input.header.value)}};
  }};
  const comments = new TaskComments(client, () => { throw Error('Unexpected write identity'); });
  const persistence = {client, comments, rootObjectId: null, reads: {getStore: () => undefined},
    localKey: id => input.prepared && id === 'local' ? 'task-board/task' : undefined,
    localDocument(key) {return key === 'task-board/operation' ? {value: structuredClone(input.operation)} :
      {...structuredClone(input.header), metadata: {parentId: 'operation'}, value: structuredClone(input.header.value)};},
    upgradeStored: TaskBoardStore.prototype.upgradeStored};
  const store = {commentSummary: (...args) => comments.summary(...args), commentRange: (...args) => comments.range(...args),
    failedHandoff(runId) {trace.push({method: 'failedHandoff', runId}); if (input.failCondition) throw raw; return null;},
    async read(key, address, ...flags) {
      trace.push({method: 'read', key, id: structuredClone(address), flags});
      const id = typeof address === 'string' ? address : address.objectId;
      if (id === input.stalled) return await new Promise(() => {});
      await Promise.resolve();
      if (id === input.failRead) throw raw;
      return await TaskBoardStore.prototype.read.call(persistence, key, address, ...flags);
    }};
  let output;
  try {
    if (entry.kind === 'assess') output = {value: await assessSteps(store, input.task, AT)};
    else if (entry.kind === 'validate') {await validateNextSteps(store, null, input.steps, input.task, AT); output = {value: null};}
    else if (entry.kind === 'store') {
      const document = await store.read('task-board/task', input.prepared ? 'local' : input.header.pin, true, true, !!input.retained);
      output = {value: document.value};
    } else output = {value: await readTask(store, input.header, input.query ?? {},
      (value, steps) => TaskBoardEngine.prototype.condition.call({store}, value, steps))};
  } catch (error) {
    if (Object.hasOwn(input, 'throwValue') && error === raw) output = {source: structuredClone(raw), identity: true};
    else if (error instanceof IvyError) output = {ivy: error.toWire()};
    else {assert.notEqual(error?.name, 'ReferenceError'); output = {javascript: {kind: error.name, message: error.message}};}
  }
  assert.equal(JSON.stringify(input), before);
  results.push({...entry, expected: output, trace, clocks});
}
const paths = ['services/task-board/src/runtime/next-steps.js', 'services/task-board/src/runtime/task-read.js',
  'services/task-board/src/runtime/comment-store.js', 'services/task-board/src/runtime/store.js',
  'services/task-board/src/runtime/engine.js', 'packages/contracts/src/task-board-questions.js', 'packages/contracts/src/task-board-records.js'];
const pins = paths.map(path => {const bytes = fs.readFileSync(resolve(root, path)); return {path, byteLength: bytes.length, contentHash: hash(bytes)};});
if (reused) assert.deepEqual(reused.pins, pins);
const output = {format: 'task-board-lossless-read-port-original-v1', at: AT, node, pins, cases: results};
fs.writeFileSync(values.output, JSON.stringify(output, null, 2) + '\n');
fs.writeFileSync(values.proof, JSON.stringify({at: new RealDate().toISOString(), node: process.version, pins,
  sourceRootWasNotExecutedAsService: true, cases: results.length, executed, reused: reusedCount,
  fixture: {byteLength: fs.statSync(values.output).size, contentHash: hash(fs.readFileSync(values.output))}}, null, 2) + '\n');
console.log(JSON.stringify({cases: results.length, sourceIdentity: results.filter(c => c.expected.identity).length,
  javascript: results.filter(c => c.expected.javascript).length, values: results.filter(c => c.expected.value).length}));
