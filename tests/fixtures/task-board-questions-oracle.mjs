import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {readFileSync} from 'node:fs';
import {stripTypeScriptTypes} from 'node:module';

const hash = bytes => 'sha256:' + createHash('sha256').update(bytes).digest('hex');
const source = readFileSync(new URL('./task-board-questions.ts.txt', import.meta.url));
const errors = readFileSync(new URL('../../packages/contracts/src/errors.ts', import.meta.url));
const sourceHash = hash(source), errorsHash = hash(errors);
assert.equal(sourceHash, 'sha256:7d1761044a9dc172f2fd7a1f7c83075b4084c4563fd31c4dee7a4bc2ae927de4');
assert.equal(errorsHash, 'sha256:44c4c25349a3820a04a383bb865e7e98782b6c3fa1ca7abbda3b33540695d517');
const load = async bytes => import('data:text/javascript;base64,' + Buffer.from(
  stripTypeScriptTypes(bytes.toString('utf8'), {mode: 'transform'}),
).toString('base64'));
const {openTaskQuestions} = await load(source);
const {IvyError} = await load(errors);
const q = (commentId = 'q', extra = {}) => ({
  commentId, purpose: 'question', authorKind: 'agent', requests: [], responses: [],
  recipient: {kind: 'user'}, ...extra,
});
const reply = (extra = {}) => ({
  commentId: 'a', purpose: 'update', authorKind: 'agent', requests: [], responses: [], ...extra,
});
const native = (commentId = 'n', extra = {}) => q(commentId, {
  nativeInput: {state: 'pending'}, run: {objectId: 'run'}, ...extra,
});
const cases = [];
const add = (name, comments, runId) => cases.push({name, task: {comments}, ...(runId === undefined ? {} : {runId})});

// The 23 captured resolver cases live in task-board-questions.json. These cases
// exercise additional boundaries, using the exact shared source above as oracle.
add('array order precedes sequence and full records survive', [
  q('later-id', {sequence: 100, body: '\ud800body\udfff', '\ud801': {raw: ['\udfff', null, 1.25]}}),
  q('earlier-id', {sequence: 1, extra: {requestLike: {requestId: 'x'}}}),
]);
add('earlier native completion does not close a later question', [
  reply({replyTo: 'n', nativeInput: {state: 'answered'}}), native(),
]);
add('native own answered state is not a later completion', [native('n', {nativeInput: {state: 'answered'}})]);
add('native own expired state is not a later completion', [native('n', {nativeInput: {state: 'expired'}})]);
add('native responses cannot replace addressed native completion', [
  native('n', {requests: [{requestId: 'r'}]}), reply({responses: [{requestId: 'r'}]}),
]);
add('native completion wrong address remains open', [native(), reply({replyTo: 'other', nativeInput: {state: 'answered'}})]);
add('native completion does not require user author or matching run', [
  native(), reply({replyTo: 'n', authorKind: 'system', run: {objectId: 'other'}, nativeInput: {state: 'expired'}}),
], 'run');
add('empty native input object is truthy', [native('n', {nativeInput: {}})], 'other');
add('missing native run is excluded only by a nonempty filter', [native('n', {run: null})], 'run');
add('missing native run survives empty filter', [native('n', {run: null})], '');
add('UTF16 replacement character cannot match lone surrogate run', [native('n', {run: {objectId: '\ud800'}})], '\ufffd');
add('UTF16 task identity requires exact source units', [
  q('q-\udfff', {recipient: {kind: 'task', taskId: 'owner-\ud800'}}),
  reply({replyTo: 'q-\udfff', sourceTaskId: 'owner-\ufffd'}),
]);
add('UTF16 task addressed reply closes', [
  q('q-\udfff', {recipient: {kind: 'task', taskId: 'owner-\ud800'}}),
  reply({replyTo: 'q-\udfff', sourceTaskId: 'owner-\ud800'}),
]);
add('UTF16 explicit resolution is exact', [q('\ud800'), reply({resolvesQuestions: ['\ufffd']})]);
add('UTF16 explicit resolution closes exact units', [q('\ud800'), reply({resolvesQuestions: ['\ud800']})]);
add('earlier response cannot cover later request', [reply({responses: [{requestId: 'r'}]}), q('q', {requests: [{requestId: 'r'}]}), reply()]);
add('duplicate requests may share a response', [q('q', {requests: [{requestId: 'r'}, {requestId: 'r'}]}), reply({responses: [{requestId: 'r'}]})]);
add('response identity is strict and not coerced', [q('q', {requests: [{requestId: '1'}]}), reply({responses: [{requestId: 1}]})]);
add('response UTF16 identity is exact across later comments', [
  q('q', {requests: [{requestId: '\ud800'}, {requestId: '\udfff'}]}),
  reply({responses: [{requestId: '\ud800'}]}), reply({responses: [{requestId: '\udfff'}]}),
]);
add('object request identities are not structurally equal', [q('q', {requests: [{requestId: {id: 'r'}}]}), reply({responses: [{requestId: {id: 'r'}}]})]);
add('falsy source identity does not hide an own question', ['', false, 0, null].map((sourceTaskId, index) => q('q' + index, {sourceTaskId})));
add('truthy source identity hides foreign questions', ['x', true, [], {}].map((sourceTaskId, index) => q('q' + index, {sourceTaskId})));
add('empty requests do not close vacuously', [q(), reply({responses: [{requestId: 'r'}]})]);
add('unknown purpose is ignored without accessing request paths', [{purpose: 'other'}]);
add('native branch does not inspect ordinary malformed paths', [native(), reply({replyTo: 'n', nativeInput: {state: 'answered'}, resolvesQuestions: 1})]);
add('no later comments avoids missing requests access', [{purpose: 'question', commentId: 'q'}]);
add('explicit resolution short circuits missing requests', [{purpose: 'question', commentId: 'q'}, {resolvesQuestions: ['q']}]);
add('addressed reply short circuits missing requests and responses', [{purpose: 'question', commentId: 'q'}, {replyTo: 'q', authorKind: 'user'}]);
add('missing requests fails only when coverage is reached', [{purpose: 'question', commentId: 'q'}, reply()]);
add('missing response array fails instead of implying no questions', [q('q', {requests: [{requestId: 'r'}]}), {purpose: 'update'}]);
add('malformed later response is skipped after coverage succeeds', [q('q', {requests: [{requestId: 'r'}]}), reply({responses: [{requestId: 'r'}]}), {purpose: 'update'}]);
add('malformed earlier response is not skipped for later coverage', [q('q', {requests: [{requestId: 'r'}]}), {purpose: 'update'}, reply({responses: [{requestId: 'r'}]})]);
add('malformed explicit resolutions fail before addressed answer', [q(), reply({resolvesQuestions: 1, replyTo: 'q', authorKind: 'user'})]);
add('nullable resolutions fall through to addressed answer', [q(), reply({resolvesQuestions: null, replyTo: 'q', authorKind: 'user'})]);
add('raw string resolutions use UTF16 includes', [q('\ud800'), reply({resolvesQuestions: 'prefix\ud800suffix'})]);
add('raw false request length does not call every', [q('q', {requests: {length: false}}), reply()]);
add('raw true request length reaches invalid every', [q('q', {requests: {length: true}}), reply()]);
add('raw string request length is coerced before invalid every', [q('q', {requests: {length: '1'}}), reply()]);
add('empty raw string request list does not call every', [q('q', {requests: ''}), reply()]);
add('nonempty raw string request list reaches invalid every', [q('q', {requests: 'r'}), reply()]);
add('null comment fails instead of disappearing', [null]);
cases.push({name: 'missing comments fails instead of an empty view', task: {}});
cases.push({name: 'null task fails instead of an empty view', task: null});
cases.push({name: 'non-array comments fail instead of an empty view', task: {comments: {}}});

for (const row of cases) {
  const before = JSON.stringify(row.task);
  row.expected = {};
  for (const mode of ['all', 'run']) {
    try {
      row.expected[mode] = {value: openTaskQuestions(row.task, mode === 'run' ? row.runId : undefined)};
    } catch (error) {
      row.expected[mode] = {error: IvyError.from(error).toWire(), originalError: error.name, originalMessage: error.message};
    }
    assert.equal(JSON.stringify(row.task), before);
  }
}
process.stdout.write(JSON.stringify({sourceHash, errorsHash, cases}));
