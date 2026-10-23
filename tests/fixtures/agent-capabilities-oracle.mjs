import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import ts from 'typescript';
import {IvyError, requireThat} from '../../dist/packages/contracts/src/errors.js';
import {canonical, hashJson} from '../../dist/packages/contracts/src/canonical.js';
import {validateAgent} from '../../dist/packages/contracts/src/agent-validation.js';

if (process.argv[2] === '--read-db') {
  const {LocalAgentState} = await import('../../dist/services/agent-manager/src/hive-state.js');
  const state = new LocalAgentState(process.argv[3], 'agent', {maxOperations: 10, maxJournalBytes: 16 * 1024 * 1024});
  try {console.log(JSON.stringify(await state.read('capabilities', 'profile')));} finally {state.close();}
  process.exit(0);
}

// Evaluate the actual private startup/handlers without starting the service.
const source = ts.createSourceFile('main.ts', readFileSync('services/agent-manager/src/main.ts', 'utf8'), ts.ScriptTarget.Latest, true);
function extract(kind, name) {
  const found = [];
  function visit(node) {
    if (kind === 'variable' && ts.isVariableDeclaration(node) && ts.isIdentifier(node.name) && node.name.text === name) found.push(node);
    if (kind === 'if' && ts.isIfStatement(node) && node.expression.getText(source) === name) found.push(node);
    if (kind === 'property' && ts.isPropertyAssignment(node) && node.name.getText(source).replaceAll('"', '') === name) found.push(node.initializer);
    ts.forEachChild(node, visit);
  }
  visit(source); assert.equal(found.length, 1, name); return found[0].getText(source);
}
const expressions = {
  open: 'async () => { let ' + extract('variable', 'capabilityState') + '; ' + extract('if', '!capabilityState') + '; return capabilityState; }',
  ...Object.fromEntries(['capabilities', 'environmentDefaults', 'configureCapabilities'].map(name => [name, extract('property', 'agent.' + name)])),
};
const globals = {IvyError, requireThat, hashJson, validateAgent};
const evaluated = Object.fromEntries(Object.entries(expressions).map(([name, text]) => [name,
  Function('env', ...Object.keys(globals), 'with(env){' + ts.transpileModule('const value=' + text + ';',
    {compilerOptions: {target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext}}).outputText + '; return value;}')]));
const evaluate = (name, env) => evaluated[name](env, ...Object.values(globals));
const c = (action, fields = {}) => ({action, ...fields});
const profile = c('profile'), defaults = c('defaults'), snapshot = c('snapshot'), open = c('open');
const cap = (label = 'Work', key = 'work') => ({key, label});
const args = (operationId = 'one', capabilities = [cap()], expectedRevision = 1) => ({operationId, expectedRevision, capabilities});
const configure = (input = args(), context = {operationId: input.operationId}, extra = {}) => c('configure', {input, context, ...extra});
const start = (handle, input = args()) => c('start', {handle, input, context: {operationId: input.operationId}});
const wait = handle => c('wait', {handle}), gate = name => c('gate', {name}), entered = name => c('entered', {name}), release = name => c('release', {name});
const value = (overrides = {}) => ({serviceNodeId: 'agent', hostId: 'host', revision: 1, capabilities: [cap()], updatedAt: 'retained', operationId: null, ...overrides});
const cases = [];
function add(name, commands, options = {}) { cases.push({name, commands, ...options}); }
add('startup-defaults-to-empty-without-normalizing', [open, profile, snapshot]);
add('startup-null-setting-defaults-to-empty', [open, profile, snapshot], {capabilities: null});
add('startup-clones-settings-without-trimming', [open, profile, c('mutateResult'), profile, snapshot], {capabilities: [cap('  Original\ufeff')], defaults: {nested: ['unchanged']}});
add('saved-profile-is-retained-without-validation-or-rewrite', [open, profile, snapshot], {saved: value({hostId: 'old-host', capabilities: [cap('  retained ')]}), capabilities: [cap('different')]});
add('saved-json-is-not-silently-repaired', [open, profile, snapshot], {saved: {retained: '\ud800', revision: 7}});
add('saved-null-keeps-original-boundary-failure-on-configuration', [open, profile, configure(), snapshot], {saved: null});
add('environment-defaults-are-independent-clones', [open, defaults, c('mutateResult'), defaults, snapshot], {defaults: {nested: ['unchanged']}});
add('update-normalizes-and-replay-ignores-new-expected-revision', [open, configure(args('one', [cap(' \ufeffWork\t')])), c('now'), configure(args('one', [cap()], 77)), profile, snapshot]);
add('ready-notification-once-and-operation-conflict', [open, c('ready', {value: true}), configure(), configure(), configure(args('one', [cap('Other')])), snapshot]);
add('new-operation-stale-revision', [open, configure(), configure(args('two', [cap()], 1)), snapshot]);
add('only-the-current-operation-is-replayed', [open, configure(), configure(args('two', [cap('Second')], 2)), configure(args('one', [cap('Later')], 3)), snapshot]);
add('profile-operation-does-not-apply-native-caller-reservations', [open, configure(args(), {operationId: 'one', callerPrincipalId: 'ivy-native-clock'}), snapshot]);
add('operation-identity-keeps-lone-utf16-units', [open, configure(args('op-\ud800')), configure(args('op-\ud800', [cap()], 17)), snapshot]);
add('second-operation-replaces-profile-and-uses-configured-identities', [open, configure(), c('now'), configure(args('two', [cap('Second', 'other')], 2)), snapshot], {saved: value({serviceNodeId: 'old-agent', hostId: 'old-host'})});
add('empty-profile-is-valid', [open, configure(args('empty', [])), snapshot]);
add('cached-read-ignores-arguments-and-context-cancellation', [open, c('profile', {input: null, cancelled: true}), c('defaults', {input: 7, cancelled: true}), configure(args(), {operationId: 'one'}, {cancelled: true}), snapshot]);
for (const input of [null, {}, {...args(), extra: true}, {...args(), expectedRevision: 0}, {...args(), expectedRevision: 1.5}, {...args(), expectedRevision: 9007199254740992}, args('one', [cap('', 'work')]), args('one', [cap('Work', 'Invalid')]), args('one', [cap('x'.repeat(129))]), args('one', Array.from({length: 65}, (_, i) => cap('Work', 'key-' + i)))]) {
  add('schema-rejection-before-context-' + cases.length, [open, configure(input, {}), snapshot]);
}
for (const context of [{}, {operationId: ''}, {operationId: 'other'}]) add('outer-identity-before-trimming-' + cases.length,
  [open, configure(args('one', [cap(' ')]), context), snapshot]);
add('duplicate-keys-are-rejected-before-replay', [open, configure(), configure(args('one', [cap(), cap('Other')])), snapshot]);
add('key-order-and-array-order-remain-significant', [open, configure(args('one', [cap('A', 'a'), cap('B', 'b')])), configure(args('one', [cap('B', 'b'), cap('A', 'a')])), snapshot]);
const spaces = [9, 10, 11, 12, 13, 32, 0xa0, 0x1680, 0x2000, 0x2001, 0x2002, 0x2003, 0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200a, 0x2028, 0x2029, 0x202f, 0x205f, 0x3000, 0xfeff];
add('every-ecmascript-space-trims-at-the-boundaries', [open, configure(args('one', spaces.map((unit, i) => cap(String.fromCharCode(unit) + 'keep' + String.fromCharCode(unit), 'key-' + i)))), snapshot]);
for (const label of [' \ufeff\t', '\u0085', '\u180e', '\u200b', ' \ud800\udfff\ud800 ', 'x\u00a0x', '😀'.repeat(128)]) add('label-unicode-boundary-' + cases.length, [open, configure(args('one', [cap(label)])), snapshot]);
add('maximum-capability-count', [open, configure(args('one', Array.from({length: 64}, (_, i) => cap('Work', 'key-' + i)))), snapshot]);
add('revision-arithmetic-retains-original-storage-number-boundary', [open, configure(args('one', [cap()], 9007199254740991)), snapshot], {saved: value({revision: 9007199254740991})});
add('startup-read-failure-is-not-an-empty-profile', [c('readFailure'), open, snapshot, open, snapshot]);
for (const afterCommit of [false, true]) add('startup-write-failure-' + afterCommit, [c('writeFailure', {afterCommit}), open, snapshot, open, snapshot]);
add('write-failure-before-commit-keeps-cache-and-allows-explicit-retry', [open, c('writeFailure', {afterCommit: false}), configure(), profile, snapshot, configure(), snapshot]);
add('write-failure-after-commit-never-retries-or-overwrites', [open, c('writeFailure', {afterCommit: true}), configure(), profile, snapshot, configure(), snapshot, open, configure(), snapshot]);
add('notification-failure-after-commit-updates-cache-before-error', [open, c('ready', {value: true}), c('notificationFailure'), configure(), profile, configure(), snapshot]);
add('external-write-does-not-refresh-cached-profile', [open, c('externalWrite', {value: value({revision: 2, operationId: 'external'})}), profile, configure(), snapshot, open, profile, snapshot]);
for (const sameOperation of [false, true]) add('concurrent-updates-compete-at-storage-' + sameOperation, [open, gate('first'), start('first'), entered('first'), c('pending', {handle: 'first'}), start('second', args(sameOperation ? 'one' : 'two', [cap('Second')])), wait('second'), release('first'), wait('first'), profile, snapshot]);

let fixedTime = '2026-10-18T00:00:00.000Z';
const RealDate = Date;
globalThis.Date = class extends RealDate { constructor(...values) { super(...(values.length ? values : [fixedTime])); } static now() { return RealDate.parse(fixedTime); } };
try {
  for (const row of cases) {
    fixedTime = '2026-10-18T00:00:00.000Z';
    const objectId = hashJson(['agent', 'capabilities', 'profile']).slice(7);
    let saved = row.saved === undefined ? null : {value: structuredClone(row.saved), pin: {objectId, revision: 1}};
    let nextFailure = null, failRead = false, failNotification = false, writeGate = null, reads = 0, last;
    const writes = [], notifications = [], gates = new Map(), handles = new Map();
    function barrier(name) { if (!gates.has(name)) { let enter, release; const entered = new Promise(r => {enter = r;}), opened = new Promise(r => {release = r;}); gates.set(name, {enter, release, entered, opened}); } return gates.get(name); }
    const state = {
      read: async (kind, key) => { assert.equal(kind, 'capabilities'); assert.equal(key, 'profile'); reads++; if (failRead) {failRead = false; throw new IvyError('service_unavailable', 'Fixture read unavailable.');} return structuredClone(saved); },
      write: async (kind, key, value, previous) => {
        assert.equal(kind, 'capabilities'); assert.equal(key, 'profile');
        writes.push({value: structuredClone(value), previous: structuredClone(previous ?? null)});
        canonical(value);
        const failure = nextFailure; nextFailure = null; const waiting = writeGate; writeGate = null;
        if (waiting) {const b = barrier(waiting); b.enter(); await b.opened;}
        if (failure && !failure.afterCommit) throw new IvyError('service_unavailable', 'Fixture write unavailable.');
        requireThat(previous ? previous.objectId === objectId && saved?.pin.revision === previous.revision : !saved, 'revision_conflict', 'Agent workflow row changed.');
        saved = {value: structuredClone(value), pin: {objectId, revision: (saved?.pin.revision ?? 0) + 1}};
        if (failure) throw new IvyError('service_unavailable', 'Fixture write unavailable.');
        return structuredClone(saved);
      },
    };
    const env = {state, config: {serviceNodeId: 'agent', hostId: 'host'}, settings: {capabilities: row.capabilities}, defaults: structuredClone(row.defaults ?? {}), capabilityState: null, track: work => work,
      service: {ready: false, connection: {notification(namespace, name, version, value) { notifications.push({namespace, name, version, value: structuredClone(value)}); if (failNotification) {failNotification = false; throw new IvyError('service_unavailable', 'Fixture notification unavailable.');} }}}};
    const handlers = Object.fromEntries(['capabilities', 'environmentDefaults', 'configureCapabilities'].map(name => [name, evaluate(name, env)]));
    const call = step => handlers.configureCapabilities(step.input, {callerPrincipalId: 'caller', signal: step.cancelled ? AbortSignal.abort(new IvyError('fixture_cancelled', 'Cancelled.')) : new AbortController().signal, ...step.context});
    const capture = async work => {try {return {result: await work() ?? null};} catch (error) {return {error: IvyError.from(error).toWire()};}};
    row.expected = [];
    for (const step of row.commands) {
      const outcome = await capture(async () => {
        switch (step.action) {
          case 'open': env.capabilityState = await evaluate('open', env)(); return env.capabilityState.value;
          case 'profile': return handlers.capabilities(step.input, step.context);
          case 'defaults': return handlers.environmentDefaults(step.input, step.context);
          case 'mutateResult': if (last.capabilities) last.capabilities[0].label = 'changed'; else last.nested[0] = 'changed'; break;
          case 'configure': return call(step);
          case 'start': {const h = {settled: false}; h.promise = capture(() => call(step)).then(value => {h.settled = true; return value;}); handles.set(step.handle, h); break;}
          case 'wait': return handles.get(step.handle).promise;
          case 'pending': return !handles.get(step.handle).settled;
          case 'gate': writeGate = step.name; barrier(step.name); break;
          case 'entered': await barrier(step.name).entered; break;
          case 'release': barrier(step.name).release(); break;
          case 'readFailure': failRead = true; break;
          case 'writeFailure': nextFailure = {afterCommit: step.afterCommit}; break;
          case 'notificationFailure': failNotification = true; break;
          case 'ready': env.service.ready = step.value; break;
          case 'now': fixedTime = '2026-10-18T01:02:03.004Z'; break;
          case 'externalWrite': await state.write('capabilities', 'profile', step.value, saved.pin); break;
          case 'snapshot': return {saved: structuredClone(saved), cached: structuredClone(env.capabilityState?.value ?? null), reads, writes: structuredClone(writes), notifications: structuredClone(notifications)};
          default: throw Error(step.action);
        }
        return null;
      });
      if (outcome.result !== null && outcome.result !== undefined && ['open', 'profile', 'defaults', 'configure'].includes(step.action)) last = outcome.result;
      row.expected.push(structuredClone(outcome));
    }
  }
  console.log(JSON.stringify({expressions, spaces, cases}));
} finally {globalThis.Date = RealDate;}
