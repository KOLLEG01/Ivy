import {registerHooks} from 'node:module';
import {existsSync, mkdtempSync, mkdirSync, readFileSync, writeFileSync, unlinkSync, symlinkSync, realpathSync, rmSync} from 'node:fs';
import {readFile, readdir, lstat} from 'node:fs/promises';
import {fileURLToPath} from 'node:url';
import {join, dirname} from 'node:path';
import {tmpdir} from 'node:os';
import {createHash} from 'node:crypto';
import ts from 'typescript';

registerHooks({resolve(specifier, context, next) {
  if (specifier.startsWith('.') && specifier.endsWith('.js') && context.parentURL) {
    const source = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
    if (existsSync(fileURLToPath(source))) return next(source.href, context);
  }
  return next(specifier, context);
}});
const {readUiBundle, publishUi} = await import('../../packages/cli/src/publish-ui.ts');
const {IvyError, requireThat} = await import('../../packages/contracts/src/errors.ts');
const {deriveOperationId} = await import('../../packages/contracts/src/operation-id.ts');
const sourcePath = new URL('../../packages/host-runtime/src/runtime-reset.ts', import.meta.url);
const source = readFileSync(sourcePath, 'utf8');
const tree = ts.createSourceFile('runtime-reset.ts', source, ts.ScriptTarget.Latest, true);
const names = ['exists', 'resetUiBundles', 'republishResetUis', 'resetBootstrapContracts'];
const functions = names.map(name => {
  const node = tree.statements.find(node => ts.isFunctionDeclaration(node) && node.name?.text === name);
  if (!node) throw Error('Missing original function: ' + name);
  return {name, text: node.getText(tree)};
});
const output = ts.transpileModule(functions.map(f => f.text).join('\n'), {
  compilerOptions: {target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS},
}).outputText;
const reference = {};
let chatCatalog = [], taskBoardCatalog = [];
new Function('exports', 'readUiBundle', 'publishUi', 'IvyError', 'requireThat', 'deriveOperationId',
  'join', 'readdir', 'lstat', 'readFile', 'chatContracts', 'taskBoardContracts', output)(reference, readUiBundle, publishUi, IvyError,
  requireThat, deriveOperationId, join, readdir, lstat, readFile, () => chatCatalog, () => taskBoardCatalog);

const parent = realpathSync(process.env.IVY_TEST_TEMP ?? tmpdir());
const root = mkdtempSync(join(parent, 'ivy-reset-ui-'));
const capture = async fn => {
  try { return {ok: true, value: await fn()}; }
  catch (error) { return {ok: false, error: IvyError.from(error).toWire()}; }
};
const contract = key => ({key, version: '1.0.0', owner: {kind: 'agent'}, mediaType: 'text/plain',
  retention: {objects: {mode: 'retain'}, revisions: {mode: 'all'}}, specMarkdown: 'Reset fixture.'});
const definition = uiId => ({metadata: {uiId, displayName: 'Reset fixture', description: 'Original reset fixture.', iconKey: 'ui'},
  entryPath: 'index.html', requirements: {hiveProtocol: 1, contracts: [], services: []}, dataContracts: []});
let sequence = 0;
function distribution() { return join(root, String(++sequence)); }
function bundle(dist, uiId, selected = definition(uiId)) {
  const directory = join(dist, 'dist', 'uis', uiId);
  mkdirSync(directory, {recursive: true});
  writeFileSync(join(directory, 'ivy-ui.json'), JSON.stringify(selected));
  writeFileSync(join(directory, 'index.html'), '<!doctype html><h1>Reset fixture</h1>');
  return directory;
}
const scans = [], publications = [], selections = [];
async function scan(name, uiIds, configure = dist => { bundle(dist, 'first'); }, transform = dist => dist) {
  const dist = distribution(); configure(dist);
  const directory = transform(dist);
  scans.push({name, distribution: directory, uiIds: JSON.stringify(uiIds),
    expected: JSON.stringify(await capture(() => reference.resetUiBundles(directory, uiIds)))});
}
try {
  await scan('configured-order', ['second', 'first'], dist => { bundle(dist, 'first'); bundle(dist, 'second'); bundle(dist, 'ignored'); });
  await scan('empty', []);
  await scan('duplicate', ['first', 'first']);
  await scan('over-32', Array.from({length: 33}, (_, i) => 'ui-' + i));
  await scan('exact-32', Array.from({length: 32}, (_, i) => 'ui-' + i), dist => { for (let i = 0; i < 32; i++) bundle(dist, 'ui-' + i); });
  await scan('absent-root', ['first'], () => {});
  await scan('missing-bundle', ['missing']);
  await scan('missing-surrogate', ['missing-\ud800']);
  await scan('bundle-is-file', ['first'], dist => { mkdirSync(join(dist, 'dist', 'uis'), {recursive: true}); writeFileSync(join(dist, 'dist', 'uis', 'first'), 'x'); });
  await scan('linked-bundle', ['first'], dist => { const target = bundle(dist, 'target'); symlinkSync(target, join(dist, 'dist', 'uis', 'first'), process.platform === 'win32' ? 'junction' : 'dir'); });
  await scan('missing-definition', ['first'], dist => { const dir = bundle(dist, 'first'); unlinkSync(join(dir, 'ivy-ui.json')); });
  for (const [name, selected] of [
    ['mismatched-ui', definition('other')], ['null-definition', null], ['missing-metadata', {}],
    ['null-metadata', {...definition('first'), metadata: null}],
    ['missing-contracts', {...definition('first'), dataContracts: undefined}],
    ['non-array-contracts', {...definition('first'), dataContracts: {}}],
    ['null-contract', {...definition('first'), dataContracts: [null]}],
    ['null-owner', {...definition('first'), dataContracts: [{owner: null}]}],
    ['service-owner', {...definition('first'), dataContracts: [{owner: {kind: 'service', serviceName: 'fixture'}}]}],
    ['short-circuit-owner', {...definition('first'), dataContracts: [{owner: {kind: 'service'}}, null]}],
    ['agent-contract', {...definition('first'), dataContracts: [contract('fixture/reset')]}],
  ]) await scan(name, ['first'], dist => { bundle(dist, 'first', selected); });
  for (const [name, bytes] of [['malformed-json', '{'], ['json-bom', '\ufeff' + JSON.stringify(definition('first'))],
    ['lossy-json-text', Buffer.concat([Buffer.from(JSON.stringify(definition('first')).split('Original reset fixture.')[0]), Buffer.from([255]), Buffer.from(JSON.stringify(definition('first')).split('Original reset fixture.')[1])])]]) {
    await scan(name, ['first'], dist => { const dir = bundle(dist, 'first'); writeFileSync(join(dir, 'ivy-ui.json'), bytes); });
  }
  await scan('definition-directory', ['first'], dist => { const dir = bundle(dist, 'first'); unlinkSync(join(dir, 'ivy-ui.json')); mkdirSync(join(dir, 'ivy-ui.json')); });
  await scan('missing-entry', ['first'], dist => { const dir = bundle(dist, 'first'); unlinkSync(join(dir, 'index.html')); });
  await scan('dot-segments', ['first'], undefined, dist => dist + '/unused/../.');

  const lost = {code: 'outcome_unknown', message: 'Synthetic lost reply.', outcome: 'unknown'};
  const absent = {code: 'not_found', message: 'Synthetic missing UI.', outcome: 'not_executed'};
  function scripted(script, trace) {
    let inspections = 0;
    return {async request(method, params) {
      trace.push({method, params});
      if (method === script.failureMethod) throw new IvyError(lost.code, lost.message, lost.outcome);
      const reply = method === 'uis.inspect' ? script.inspections[inspections++] : {result: {fixture: true, method}};
      if (!reply) throw Error('Missing scripted response.');
      if (reply.error) throw new IvyError(reply.error.code, reply.error.message, reply.error.outcome);
      return reply.result;
    }};
  }
  async function publication(name, configureScript, {ids = ['first'], mutate, mutationId = 'reset-epoch:1792500000000:retained-reset'} = {}) {
    const dist = distribution();
    for (const id of ids) bundle(dist, id, {...definition(id), dataContracts: [contract('fixture/' + id)]});
    const bundles = ids.length ? await reference.resetUiBundles(dist, ids) : [];
    const script = configureScript(bundles);
    if (mutate) mutate(bundles);
    const trace = [];
    const expected = await capture(() => reference.republishResetUis(scripted(script, trace), bundles, mutationId));
    publications.push({name, bundles: JSON.stringify(bundles), script: JSON.stringify(script), mutationId: JSON.stringify(mutationId),
      expected: JSON.stringify(expected), trace: JSON.stringify(trace)});
  }
  const fresh = () => ({inspections: [{error: absent}, {result: {status: 'ready'}}]});
  await publication('fresh', fresh);
  await publication('null-inspection', () => ({inspections: [{result: null}, {result: {status: 'ready'}}]}));
  await publication('false-inspection', () => ({inspections: [{result: false}, {result: {status: 'ready'}}]}));
  await publication('ready-resume-no-files', b => ({inspections: [{result: {status: 'ready', currentReleaseId: b[0].checkedBundle.releaseId}}]}),
    {mutationId: 'not-an-operation-id', mutate: b => { unlinkSync(join(b[0].directory, 'index.html')); }});
  await publication('conflicting-release', () => ({inspections: [{result: {status: 'ready', currentReleaseId: 'different'}}]}));
  await publication('existing-not-ready', b => ({inspections: [{result: {status: 'unavailable', currentReleaseId: b[0].checkedBundle.releaseId}}]}));
  await publication('existing-missing-release', () => ({inspections: [{result: {status: 'ready'}}]}));
  await publication('inspect-denied', () => ({inspections: [{error: {code: 'forbidden', message: 'Synthetic denied UI.', outcome: 'not_executed'}}]}));
  await publication('invalid-parent', fresh, {mutationId: 'invalid'});
  await publication('empty-no-parent-check', () => ({inspections: []}), {ids: [], mutationId: 'invalid'});
  for (const failureMethod of ['contracts.register', 'uis.stageAsset', 'uis.deploy'])
    await publication('lost-' + failureMethod, () => ({...fresh(), failureMethod}));
  await publication('verification-lost', () => ({inspections: [{error: absent}, {error: lost}]}));
  await publication('verification-not-ready', () => ({inspections: [{error: absent}, {result: {status: 'staging'}}]}));
  await publication('verification-null', () => ({inspections: [{error: absent}, {result: null}]}));
  await publication('verification-status-only', () => ({inspections: [{error: absent}, {result: {status: 'ready', currentReleaseId: 'unchecked-original-field'}}]}));
  await publication('changed-asset', fresh, {mutate: b => { writeFileSync(join(b[0].directory, 'index.html'), 'changed'); }});
  await publication('ordered-two', b => ({inspections: [{result: {status: 'ready', currentReleaseId: b[0].checkedBundle.releaseId}}, {error: absent}, {result: {status: 'ready'}}]}), {ids: ['second', 'first']});
  const service = (key, version = '1.0.0', tag = key) => ({...contract(key), version, owner: {kind: 'service', serviceName: 'fixture'}, specMarkdown: tag});
  const required = (key, readVersions = ['1.0.0'], writeVersions = []) => ({key, readVersions, writeVersions});
  async function selection(name, {requests = [], bundled = [], chat = [], board = [], empty = false} = {}) {
    chatCatalog = chat; taskBoardCatalog = board;
    const selected = empty ? [] : [{directory: 'unused', definition: {...definition('reset-fixture'), dataContracts: bundled,
      requirements: {hiveProtocol: 1, services: [], contracts: requests}},
      checkedBundle: {assets: [], bundleHash: 'unused', releaseId: 'unused'}}];
    selections.push({name, bundles: JSON.stringify(selected), chat: JSON.stringify(chat), board: JSON.stringify(board),
      expected: JSON.stringify(await capture(() => reference.resetBootstrapContracts(selected)))});
  }
  await selection('empty-bundles', {empty: true, chat: [service('fixture/unrequested')]});
  await selection('no-requirements');
  await selection('both-original-builder-positions', {requests: [required('fixture/task', [], ['1.0.0']), required('fixture/chat')], chat: [service('fixture/chat')], board: [service('fixture/task')]});
  await selection('agent-definition-excluded', {requests: [required('fixture/agent')], bundled: [contract('fixture/agent')]});
  await selection('bundled-agent-shadows-service', {requests: [required('fixture/shadow')], bundled: [contract('fixture/shadow')], chat: [service('fixture/shadow')]});
  await selection('bundled-service-first', {requests: [required('fixture/shadow')], bundled: [service('fixture/shadow', '1.0.0', 'bundled')], chat: [service('fixture/shadow', '1.0.0', 'chat')]});
  await selection('missing-contract', {requests: [required('fixture/missing')], chat: [service('fixture/other')]});
  await selection('exact-version-required', {requests: [required('fixture/version', ['2.0.0'])], chat: [service('fixture/version')]});
  await selection('dedup-read-write', {requests: [required('fixture/shared', ['1.0.0', '1.0.0'], ['1.0.0']), required('fixture/shared')], chat: [service('fixture/shared')]});
  await selection('chat-first-exact-definition', {requests: [required('fixture/shared')], chat: [service('fixture/shared', '1.0.0', 'chat')], board: [service('fixture/shared', '1.0.0', 'board')]});
  await selection('version-locale-not-semver', {requests: [required('fixture/version', ['1.2.0', '1.10.0'])], chat: [service('fixture/version', '1.2.0'), service('fixture/version', '1.10.0')]});
  const keys = ['fixture/a_b', 'fixture/a-b', 'fixture/a/b', 'fixture/ab'];
  await selection('key-locale-not-ordinal', {requests: keys.map(key => required(key)), chat: keys.map(key => service(key))});
  const equal = ['fixture/a\u200d', 'fixture/a', 'fixture/a\u034f'];
  await selection('stable-equal-collation', {requests: equal.map(key => required(key)), chat: equal.map(key => service(key))});
  await selection('missing-utf16-error', {requests: [required('fixture/\ud800', ['1.0.0'])]});
  await selection('unrequested-catalog-entries', {requests: [required('fixture/needed')], chat: [service('fixture/extra'), service('fixture/needed')], board: [service('fixture/other')]});
  await selection('no-empty-catalog-fallback', {requests: [required('fixture/needed')]});
  process.stdout.write(JSON.stringify({root, scans, publications, selections,
    locale: {default: new Intl.Collator().resolvedOptions().locale, icu: process.versions.icu},
    original: {path: fileURLToPath(sourcePath), sha256: createHash('sha256').update(source).digest('hex'), functions}}));
} catch (error) {
  if (dirname(realpathSync(root)) !== parent) throw Error('Unexpected fixture root.');
  rmSync(root, {recursive: true, force: true});
  throw error;
}
