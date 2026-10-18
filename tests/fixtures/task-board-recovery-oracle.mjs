import {registerHooks, stripTypeScriptTypes} from 'node:module';
import {existsSync, readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(s, c, next) {
  if (s.startsWith('.') && s.endsWith('.js') && c.parentURL) { const u = new URL(s.slice(0, -3) + '.ts', c.parentURL); if (existsSync(fileURLToPath(u))) return next(u.href, c); }
  return next(s, c);
}});
const {IvyError, requireThat} = await import('../../packages/contracts/src/errors.ts');
const {hashJson} = await import('../../packages/contracts/src/canonical.ts');
const source = readFileSync('services/task-board/src/runtime/native-recovery.ts', 'utf8');
const methods = source.slice(source.indexOf('export const missingNativeThread'), source.indexOf('async function unstartedAttempt')).replaceAll('export ', '');
const functions = new Function('requireThat', stripTypeScriptTypes(methods) + '\nreturn {missingNativeThread,contextRecoveryHistory,freshContextRecovery};')(requireThat);
const support = readFileSync('tests/fixtures/task-board-operations-oracle.mjs', 'utf8');
const Memory = new Function('hashJson', 'IvyError', support.slice(support.indexOf('class Memory {'), support.indexOf('const begin=')) + '\nreturn Memory;')(hashJson, IvyError);
const pin = (objectId, revision = 1) => ({objectId, revision});
const task = {lastRun: pin('run'), lastHistory: pin('h1'), primaryResourceRef: null};
const history = (extra = {}) => ({taskId: 'task', priorRun: pin('run'), kind: 'recovered', actor: {source: 'scheduler'}, previousHistory: null, ...extra});
const record = (id = 'h1', value = history(), key = 'task-board/history') => ({id, value, key});
const rows = [], add = (name, input = {}) => rows.push({name, input: {method: 'history', task, taskId: 'task', records: [record()], ...input}});
add('saved scheduler recovery'); add('no history', {task: {...task, lastHistory: null}}); add('no last run', {task: {...task, lastRun: null}});
add('user recovery is not scheduler recovery', {records: [record('h1', history({actor: {source: 'user'}}))]});
add('walk prior history', {records: [record('h1', history({kind: 'updated', previousHistory: pin('h2')})), record('h2')]});
add('reassignment stops lineage', {records: [record('h1', history({kind: 'reassigned', previousHistory: pin('h2')})), record('h2')]});
add('another prior run stops lineage', {records: [record('h1', history({priorRun: pin('other'), previousHistory: pin('h2')})), record('h2')]});
add('missing prior run stops lineage', {records: [record('h1', history({priorRun: null}))]});
add('prior run revision is not a different run identity', {records: [record('h1', history({priorRun: pin('run', 99)}))]});
add('foreign history refused before lineage break', {records: [record('h1', history({taskId: 'other', kind: 'reassigned'}))]});
add('self cycle', {records: [record('h1', history({kind: 'updated', previousHistory: pin('h1')}))]});
add('same identity different revision cycle', {records: [record('h1', history({kind: 'updated', previousHistory: pin('h1', 2)}))]});
add('two record cycle', {records: [record('h1', history({kind: 'updated', previousHistory: pin('h2')})), record('h2', history({kind: 'updated', previousHistory: pin('h1')}))]});
add('missing history read propagates', {records: []});
add('UTF16 lineage identity', {task: {...task, lastHistory: pin('h\ud800')}, records: [record('h\ud800')]});
const run = {taskId: 'task', phase: 'failed', turnId: null, cancellation: null};
const fresh = (name, options = {}) => add('fresh: ' + name, {method: 'fresh', records: [record('run', run, 'task-board/run'), record()], ...options});
fresh('failed empty original run'); fresh('existing primary skips reads', {task: {...task, primaryResourceRef: {kind: 'thread'}}});
fresh('empty object primary remains truthy', {task: {...task, primaryResourceRef: {}}});
fresh('missing last run skips reads', {task: {...task, lastRun: null}});
for (const phase of ['running', 'succeeded', 'cancelled']) fresh('phase ' + phase, {records: [record('run', {...run, phase}, 'task-board/run'), record()]});
fresh('turn already allocated', {records: [record('run', {...run, turnId: 'turn'}, 'task-board/run'), record()]});
fresh('cancellation retained', {records: [record('run', {...run, cancellation: {}}, 'task-board/run'), record()]});
fresh('no recovery history', {task: {...task, lastHistory: null}});
fresh('unreadable original run', {records: [record()]});
fresh('history task identity must match original run', {records: [record('run', {...run, taskId: 'other'}, 'task-board/run'), record()]});
fresh('reassigned context is not reused', {records: [record('run', run, 'task-board/run'), record('h1', history({kind: 'reassigned'}))]});
for (const row of rows) {
  const i = row.input, store = new Memory();
  for (const r of i.records) store.docs.set(r.id, {key: r.key, name: 'record', pin: pin(r.id), value: r.value, metadata: {parentId: 'root'}, revision: {contentHash: hashJson(r.value)}});
  try { row.expected = {value: await (i.method === 'fresh' ? functions.freshContextRecovery(store, i.task) : functions.contextRecoveryHistory(store, i.task, i.taskId)), trace: store.trace}; }
  catch (error) { row.expected = {value: {error: {code: error.code, message: error.message, outcome: error.outcome}}, trace: store.trace}; }
}
const messages = ['thread foo not found', 'NO ROLLOUT FOUND foo', 'foo rollout does not exist', 'foo does not exist: THREAD', 'thread foo exists', 'thread bar not found', 'prefix foo; no rollout found', 'thread foo\nnot found', 'thread foo\rdoes not exist', 'thread foo\u2028not found', 'thread foo\u2029not found', 'thread foo\vnot found', 'thread foo\u0085not found', 'foo\nthread bar not found', 'thread foo doeſ not exist', 'thread foo not found\ud800'];
const missing = messages.map(message => ({reply: {error: {message}}, threadId: 'foo'}));
missing.push({reply: null, threadId: 'foo'}, {reply: {result: {}}, threadId: 'foo'}, {reply: {error: {message: 'no rollout found'}}, threadId: ''}, {reply: {error: {message: 'thread x\ud800 not found'}}, threadId: 'x\ud800'});
for (const input of missing) { input.expected = functions.missingNativeThread(input.reply, input.threadId); }
process.stdout.write(JSON.stringify({rows, missing}));
