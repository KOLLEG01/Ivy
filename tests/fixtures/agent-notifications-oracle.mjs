import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import ts from 'typescript';
import {NativeNotifications} from '../../dist/services/agent-manager/src/notifications.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
const source = ts.createSourceFile('main.ts', readFileSync('services/agent-manager/src/main.ts', 'utf8'), ts.ScriptTarget.Latest, true);
const found = [];
function visit(node) {if (ts.isPropertyAssignment(node) && node.name.getText(source).replaceAll('"', '') === 'agent.notifications') found.push(node.initializer.getText(source)); ts.forEachChild(node, visit);}
visit(source); assert.equal(found.length, 1);
const expression = found[0];
const make = Function('notifications', 'journal', ts.transpileModule('const handler=' + expression + ';', {compilerOptions: {target: ts.ScriptTarget.ES2022}}).outputText + '; return handler;');
const observe = (params = null, extra = {}) => ({action: 'observe', params, epoch: 'epoch', method: 'turn/completed', ...extra});
const page = (afterSequence = 0, limit) => ({action: 'page', query: {afterSequence, ...(limit === undefined ? {} : {limit})}});
const cases = [], add = (name, commands, extra = {}) => cases.push({name, commands, ...extra});
add('empty-and-future-cursors', [page(), page(7), {action: 'epoch', epoch: 'epoch'}, page()]);
add('contiguous-default-and-explicit-page-sizes', [observe(), observe(false), observe({exact: '\ud800'}), page(0, 2), page(2, 2), page(3)]);
add('oversized-events-leave-gaps', [observe(), observe('x'.repeat(2048)), observe(false), page()], {maximumBytes: 1024});
add('retention-drops-oldest-by-encoded-bytes', [observe(), observe(), page()], {maximumBytes: 300});
add('sequence-exhaustion-keeps-original-boundary', [observe(), observe(), page(10), page(11)], {start: 10, end: 11});
add('journal-epoch-is-current-at-each-call', [observe(), {action: 'epoch', epoch: 'first'}, page(), {action: 'epoch', epoch: 'second'}, page(), {action: 'epoch', epoch: null}, page()]);
add('utf16-owner-epoch-method-and-payload', [observe({exact: ['\udfff', null]}, {epoch: 'native\ud800', method: 'method\udfff'}), {action: 'epoch', epoch: 'journal\ud800'}, page()], {owner: {serviceNodeId: 'node\ud800', nativeVersion: 'version\udfff'}});
for (const query of [null, [], {}, 0, {afterSequence: -1}, {afterSequence: 1.5}, {afterSequence: 9007199254740992}, {afterSequence: '0'}, {afterSequence: 0, limit: null}, {afterSequence: 0, limit: 0}, {afterSequence: 0, limit: 101}, {afterSequence: 0, limit: 1.5}, {afterSequence: 0, extra: true}]) add('invalid-query-' + cases.length, [observe(), {action: 'page', query}]);
const RealDate = Date;
globalThis.Date = class extends RealDate {constructor(...args) {super(...(args.length ? args : ['2026-10-18T00:00:00.000Z']));}};
try {
  for (const row of cases) {
    const notifications = new NativeNotifications(row.owner ?? {serviceNodeId: 'node', nativeVersion: '0.159.2'}, {start: row.start ?? 0, end: row.end ?? 9007199254740991}, row.maximumBytes ?? 4096);
    const journal = {epoch: null}, handler = make(notifications, journal);
    row.expected = [];
    for (const command of row.commands) {
      try {
        let result;
        if (command.action === 'observe') result = notifications.observe(command.epoch, command.method, command.params);
        else if (command.action === 'epoch') journal.epoch = command.epoch;
        else result = handler(command.query, {signal: AbortSignal.abort()});
        row.expected.push({result: structuredClone(result ?? null)});
      } catch (error) {row.expected.push({error: IvyError.from(error).toWire()});}
    }
  }
  console.log(JSON.stringify({expression, cases}));
} finally {globalThis.Date = RealDate;}
