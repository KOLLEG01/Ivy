import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {stripTypeScriptTypes} from 'node:module';
import {parseArgs} from 'node:util';
import {pathToFileURL} from 'node:url';

const {values} = parseArgs({options: {
  'source-root': {type: 'string'}, output: {type: 'string'}, proof: {type: 'string'}, reuse: {type: 'string'},
  'backfill-cases': {type: 'boolean'},
}});
for (const key of ['source-root', 'output', 'proof']) assert.ok(values[key], key);
const root = path.resolve(values['source-root']);
const source = file => fs.readFileSync(path.join(root, file), 'utf8');
const hash = bytes => 'sha256:' + createHash('sha256').update(bytes).digest('hex');
const {IvyError, requireThat} = await import(pathToFileURL(path.join(root, 'packages/contracts/src/errors.ts')));
const files = ['packages/contracts/src/task-board-records.ts',
  'packages/contracts/src/task-board-questions.ts', 'packages/contracts/src/errors.ts',
  'services/task-board/src/runtime/schema.ts', 'services/task-board/src/runtime/store.ts'];
const transform = text => stripTypeScriptTypes(text, {mode: 'transform'})
  .replace(/^import .*;\r?\n/gm, '').replace(/^export /gm, '');
const questions = new Function(transform(source(files[1])) + '\nreturn openTaskQuestions;')();
const upgrades = new Function('openTaskQuestions', transform(source(files[0])) +
  '\nreturn {upgradeTaskRecord,upgradeConfigurationRecord,upgradeResultRecord};')(questions);
function section(text, start, end) {
  const from = text.indexOf(start), to = text.indexOf(end, from + start.length);
  assert.ok(from >= 0 && to > from);
  return text.slice(from, to);
}
const schema = source(files[3]);
const upgradeRecordSource = section(schema, 'export function upgradeRecord<', '\n}', ) + '\n}';
const upgradeRecord = new Function(...Object.keys(upgrades), transform(upgradeRecordSource) +
  '\nreturn upgradeRecord;')(...Object.values(upgrades));
const method = section(source(files[4]), '  async upgradeStored<', '  withReadCache<');
const Original = new Function('requireThat', 'upgradeRecord',
  transform('class Original {\n' + method + '\n}') + '\nreturn Original;')(requireThat, upgradeRecord);
const task = extra => ({schemaVersion: 1, fields: {description: 'Continue. \ud800'},
  workflowState: 'todo', comments: [], updatedAt: '2026-01-01T00:00:00.000Z', ...extra});
const artifact = (id, extra = {}) => ({object: {objectId: id, revision: 2}, contentHash: 'sha256:' + id,
  mediaType: 'application/json', label: 'Evidence \udfff' + id, ...extra});
const read = (id, byteLength = 7) => ({value: {revision: {
  contentHash: 'sha256:' + id, mediaType: 'application/json', byteLength,
}}});
const finding = evidence => ({label: 'Retained finding.', detail: 'Original \ud800 detail.', evidence});
const inputs = [];
const add = (name, value, rpc = [], key = 'task-board/task') => inputs.push({name, input: {key, value, rpc}});
add('backfill precedes findings and current nextSteps short circuit', task({nextSteps: [],
  latestChecks: [finding([artifact('a')])]}), [read('a', 19)]);
add('three finding locations retain RPC and value order', task({nextSteps: [],
  latestChecks: [finding([artifact('a')])], content: {checks: [finding([artifact('b')])]},
  comments: [{commentId: 'c', sequence: 1, authorKind: 'agent',
    author: {principalId: 'original'}, purpose: 'update', body: 'Retained.', requests: [], responses: [],
    attachments: [], handoff: {workflowState: 'todo', nextSteps: [], checks: [finding([artifact('c')])]}}],
}), ['a', 'b', 'c'].map(id => read(id)));
add('equal pins are read separately before artifact dedup', task({nextSteps: [],
  latestChecks: [finding([artifact('a'), artifact('a')])]}), [read('a'), read('a')]);
add('existing zero and null sizes bypass evidence reads', task({nextSteps: [],
  latestChecks: [finding([artifact('zero', {byteLength: 0}), artifact('null', {byteLength: null})])]}));
add('missing returned byteLength stays absent', task({nextSteps: [], latestChecks: [finding([artifact('a')])]}),
  [{value: {revision: {contentHash: 'sha256:a', mediaType: 'application/json'}}}]);
add('single artifact finding is backfilled before Shared rendering', task({nextSteps: [],
  latestChecks: [finding(artifact('a'))]}), [read('a')]);
add('typed RPC failure wins before invalid Shared date', task({
  waiting: {reason: 'host', since: 'invalid', detail: 'Wait.'}, latestChecks: [finding([artifact('a')])],
}), [{error: {code: 'revision_pruned', message: 'Original \ud800 failure.', outcome: 'completed', details: {raw: '\udfff'}}}]);
add('hash refusal wins before invalid Shared date', task({
  waiting: {reason: 'host', since: 'invalid', detail: 'Wait.'}, latestChecks: [finding([artifact('a')])],
}), [read('wrong')]);
const wrongMime = read('a'); wrongMime.value.revision.mediaType = 'text/plain';
add('MIME refusal wins before invalid Shared fields', task({fields: null, latestChecks: [finding([artifact('a')])]}), [wrongMime]);
add('raw RangeError survives after successful evidence read', task({
  waiting: {reason: 'host', since: 'invalid', detail: 'Wait.'}, latestChecks: [finding([artifact('a')])],
}), [read('a')]);
add('raw TypeError survives after successful evidence read', task({fields: null,
  latestChecks: [finding([artifact('a')])]}), [read('a')]);
add('configuration dispatch bypasses Task backfill', {defaults: {executionRequirement: {kind: 'capability', capabilityKey: 'old'}},
  comments: {}, latestChecks: [finding([artifact('unread')])], '\ud800': '\udfff'}, [read('unread')], 'task-board/configuration');
add('configuration raw failure remains raw', {defaults: null}, [read('unread')], 'task-board/configuration');
add('result dispatch bypasses Task backfill', {content: {summary: 'Original.', artifacts: [], checks: []},
  comments: {}, latestChecks: [finding([artifact('unread')])], unknown: '\ud800'}, [read('unread')], 'task-board/result');
add('result raw failure remains raw', {extra: '\udfff'}, [read('unread')], 'task-board/result');
add('other contract remains exact and does not read', {comments: {}, latestChecks: null, '\ud800': '\udfff'},
  [read('unread')], 'task-board/run');
add('other contract null remains null', null, [read('unread')], 'task-board/operation');
add('current Task is an owned materialized value', task({nextSteps: [], unknown: '\udfff'}));
add('current configuration keeps original identity', {defaults: {executionRequirement: {kind: 'automatic'}},
  unknown: '\ud800'}, [], 'task-board/configuration');
add('other contract array keeps original identity', ['\ud800', {unknown: '\udfff'}], [], 'task-board/run');
add('current result remains an owned record', {content: {summary: 'Unchanged content.', artifacts: []},
  unknown: '\udfff'}, [], 'task-board/result');
if (values['backfill-cases']) {
  add('raw backfill null Task', null);
  for (const value of [0, true, {}]) add('raw latestChecks noniterable ' + JSON.stringify(value), task({latestChecks: value}));
  add('raw latestChecks string is spread before Shared', task({nextSteps: [], latestChecks: 'x\ud800'}));
  add('raw content checks noniterable', task({content: {checks: {}}}));
  add('raw content primitive optional access', task({nextSteps: [], content: 7}));
  for (const value of [{}, false, '']) add('raw comments flatMap ' + JSON.stringify(value), task({comments: value}));
  add('raw null comment precedes earlier evidence reads', task({latestChecks: [finding([artifact('unread')])], comments: [null]}), [read('unread')]);
  add('raw handoff checks flatMap accepts single finding', task({nextSteps: [], comments: [{commentId: 'c',
    sequence: 1, authorKind: 'agent', author: {principalId: 'original'}, requests: [], responses: [], attachments: [],
    handoff: {workflowState: 'todo', nextSteps: [], checks: finding([artifact('a')])}}]}), [read('a')]);
  add('raw null finding precedes earlier evidence reads', task({latestChecks: [finding([artifact('unread')]), null]}), [read('unread')]);
  add('raw null artifact access', task({latestChecks: [finding([null])]}));
  add('raw null evidence coalesces before Shared', task({nextSteps: [], latestChecks: [finding(null)]}));
  for (const value of [7, false, 'x\ud800', []]) add('raw primitive artifact ' + JSON.stringify(value),
    task({nextSteps: [], latestChecks: [finding([value])]}), [{value: {revision: {}}}]);
  add('raw missing artifact pin preserves undefined argument', task({nextSteps: [], latestChecks: [finding([{}])]}), [{value: {revision: {byteLength: 11}}}]);
  add('raw null artifact pin stays null', task({nextSteps: [], latestChecks: [finding([{object: null}])]}), [{value: {revision: {byteLength: 11}}}]);
  for (const value of [null, {}, {revision: null}]) add('raw malformed evidence response ' + JSON.stringify(value),
    task({latestChecks: [finding([artifact('a')])]}), [{value}]);
}
const cases = [];
const reuse = values.reuse ? JSON.parse(fs.readFileSync(values.reuse)) : null;
let executed = 0, reused = 0;
for (const entry of inputs) {
  const prior = reuse?.cases.find(c => c.name === entry.name);
  if (prior) { assert.deepEqual(prior.input, entry.input); cases.push(prior); reused++; continue; }
  executed++;
  const input = structuredClone(entry.input), before = JSON.stringify(input), trace = [], pending = structuredClone(input.rpc);
  let thrown = null;
  const client = {async request(method, params) {
    trace.push({method, params: structuredClone(params)});
    const next = pending.shift(); assert.ok(next, 'Unexpected original RPC');
    if (next.error) { thrown = new IvyError(next.error.code, next.error.message, next.error.outcome, next.error.details); throw thrown; }
    return structuredClone(next.value);
  }};
  let expected, sameIdentity;
  try {
    const value = await Original.prototype.upgradeStored.call({client}, input.key, input.value);
    expected = {value}; sameIdentity = value === input.value;
  }
  catch (error) {
    assert.notEqual(error.name, 'ReferenceError', 'Incomplete original module context');
    expected = error instanceof IvyError ? {ivy: error.toWire()} : {javascript: {kind: error.name, message: error.message}};
    if (thrown) assert.equal(error, thrown);
  }
  assert.equal(JSON.stringify(input), before, 'Original input must not be mutated');
  cases.push({...entry, expected, trace, rpcRemaining: pending.length, ...(sameIdentity === undefined ? {} : {sameIdentity})});
}
const sourceFiles = files.map(file => {const bytes = fs.readFileSync(path.join(root, file));
  return {path: file, byteLength: bytes.length, contentHash: hash(bytes)};});
if (reuse) assert.deepEqual(reuse.sourceFiles, sourceFiles, 'Only reuse observations from these exact originals');
const result = {format: 'task-board-record-upgrade-composition-v1', sourceFiles, cases};
fs.writeFileSync(values.output, JSON.stringify(result, null, 2) + '\n');
fs.writeFileSync(values.proof, JSON.stringify({at: new Date().toISOString(), node: process.version,
  sourceFiles, method, upgradeRecordSource, transformedRecordSourceHash: hash(Buffer.from(transform(source(files[0])))),
  executed, reused, cases: cases.length, prior70OriginalsReplayed: false,
  fixture: {byteLength: fs.statSync(values.output).size, contentHash: hash(fs.readFileSync(values.output))}}, null, 2) + '\n');
console.log(JSON.stringify({executed, reused, cases: cases.length, ivy: cases.filter(c => c.expected.ivy).length,
  javascript: cases.filter(c => c.expected.javascript).length}));
