import {registerHooks, stripTypeScriptTypes} from 'node:module';
import {existsSync, readFileSync} from 'node:fs';
import {fileURLToPath} from 'node:url';
registerHooks({resolve(s, c, next) {
  if (s.startsWith('.') && s.endsWith('.js') && c.parentURL) { const u = new URL(s.slice(0, -3) + '.ts', c.parentURL); if (existsSync(fileURLToPath(u))) return next(u.href, c); }
  return next(s, c);
}});
const {IvyError, requireThat} = await import('../../packages/contracts/src/errors.ts');
const {hashJson} = await import('../../packages/contracts/src/canonical.ts');
const ownerSource = readFileSync('packages/sdk/src/connection-owner.ts', 'utf8');
// The test client is an ordinary RpcClient. Bound ServiceConnection caching is
// supplied and qualified by the shared SDK, not reimplemented by this adapter.
const connectionOwner = new Function('ServiceConnection', stripTypeScriptTypes(ownerSource.slice(ownerSource.indexOf('const owners=')).replace('export ', '')) + '\nreturn connectionOwner;')(class ServiceConnection {});
const engine = readFileSync('services/task-board/src/runtime/engine.ts', 'utf8');
const verify = engine.slice(engine.indexOf('  async verifyOwner()'), engine.indexOf('  async invoke('));
const Owner = new Function('connectionOwner', 'requireThat', stripTypeScriptTypes('class Owner {\n' + verify + '\n}') + '\nreturn Owner;')(connectionOwner, requireThat);
const storeSource = readFileSync('services/task-board/src/runtime/store.ts', 'utf8');
const read = storeSource.slice(storeSource.indexOf('  async read<'), storeSource.indexOf('  async named<'));
const Store = new Function('localContractKeys', 'readableVersions', 'upgradeRecord', 'requireThat', 'pinOf', stripTypeScriptTypes('class Store {\n' + read + '\n}') + '\nreturn Store;')(
  new Set(), () => ['1.0.0'], (_key, value) => value, requireThat, result => ({objectId: result.object.id, revision: result.revision.revision}),
);
const inspector = readFileSync('services/task-board/src/runtime/native-inspector.ts', 'utf8').replace(/^import .*;\r?\n/gm, '').replaceAll('export ', '');
// These context cases intentionally stop at the saved-primary guard, before
// catalog/evidence or native tools. Only that guard is being qualified here.
const Inspector = new Function('requireThat', 'TaskBoardReadEvidence', stripTypeScriptTypes(inspector, {mode: 'transform'}) + '\nreturn TaskBoardNativeInspector;')(requireThat, class ReadEvidence {});
const owner = {principalId: 'caller', serviceName: 'task-board', connected: true, synced: true};
const run = {target: {serviceNodeId: 'native-agent'}, primaryResourceRef: null, turnId: null, nativeEpoch: null};
const pin = {objectId: 'run', revision: 1};
const rows = [], add = (name, input = {}) => rows.push({name, input: {principal: 'caller', serviceNodeId: 'board', owner, ...input}});
add('authenticated owner'); add('wrong principal', {owner: {...owner, principalId: 'other'}});
add('wrong service', {owner: {...owner, serviceName: 'other'}}); add('disconnected', {owner: {...owner, connected: false}});
add('unsynchronized', {owner: {...owner, synced: false}}); add('missing connectivity', {owner: {principalId: 'caller', serviceName: 'task-board'}});
add('UTF16 principal', {principal: 'caller\ud800', owner: {...owner, principalId: 'caller\ud800'}});
add('UTF16 mismatch', {principal: 'caller\ud800', owner: {...owner, principalId: 'caller\udfff'}});
add('UTF16 service-node request', {serviceNodeId: 'board\ud800'});
add('original truthy flags', {owner: {...owner, connected: [], synced: {}}});
add('owner read unavailable', {ownerError: {code: 'service_unavailable', outcome: 'unknown'}});
add('fresh ordinary-client owner after synchronization loss', {owners: [owner, {...owner, synced: false}], repeat: 2});
const ownerRows = [...rows];
for (const row of ownerRows) add('context: ' + row.name, {...row.input, context: true});
for (const row of rows) {
  const i = row.input, trace = [], outcomes = []; let owners = 0;
  i.run = run; i.pin = pin;
  i.readResult = {object: {id: 'run', parentId: 'root', contractKey: 'task-board/run', effectivelyArchived: false},
    revision: {revision: 1, contractVersion: '1.0.0', mediaType: 'application/json', contentHash: hashJson(run), byteLength: 100}, content: {encoding: 'json', value: run}};
  const client = {request: async (method, args) => {
    trace.push([method, args]);
    if (method === 'serviceNodes.get') {
      if (i.ownerError) throw new IvyError(i.ownerError.code, 'Fixture owner read.', i.ownerError.outcome);
      return structuredClone(i.owners?.[owners++] ?? i.owner);
    }
    if (method !== 'objects.read') throw Error('Unexpected RPC ' + method);
    return structuredClone(i.readResult);
  }};
  const store = new Store(); Object.assign(store, {client, rootObjectId: 'root', localKey: () => null, reads: {getStore: () => undefined}});
  const engine = new Owner(); Object.assign(engine, {store, owner: {serviceNodeId: i.serviceNodeId}, settings: {principalId: i.principal}});
  for (let n = 0; n < (i.repeat ?? 1); n++) {
    try { outcomes.push(i.context ? await new Inspector(engine).context(pin) : (await engine.verifyOwner(), null)); }
    catch (error) { outcomes.push({error: {code: error.code, message: error.message, outcome: error.outcome}}); }
  }
  row.expected = {outcomes, trace};
}
process.stdout.write(JSON.stringify(rows));
