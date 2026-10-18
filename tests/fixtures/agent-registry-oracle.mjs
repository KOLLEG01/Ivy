import assert from 'node:assert/strict';
import {readFileSync, readdirSync} from 'node:fs';
import {agentRegistry, agentSchema} from '../../dist/services/agent-manager/src/registry.js';
import {discoverNativeTools} from '../../dist/services/agent-manager/src/native-discovery.js';
import {hashJson} from '../../dist/packages/contracts/src/canonical.js';
import {IvyError} from '../../dist/packages/contracts/src/errors.js';
import {toolDefinitionHash} from '../../dist/packages/contracts/src/tool-definition.js';

const read = path => JSON.parse(readFileSync(path, 'utf8'));
const paths = readdirSync('specs/native').filter(name => /^codex-/.test(name))
  .flatMap(directory => readdirSync(`specs/native/${directory}`)
    .filter(name => /^catalog(?:\.[a-z0-9]+-[a-z0-9]+)?\.json$/.test(name))
    .map(name => `specs/native/${directory}/${name}`));
assert.equal(paths.length, 7, 'qualify every retained catalog');
const original = read(paths[0]);
const minimal = methods => ({...original, clientRequests: methods.map(method => ({
  ...original.clientRequests[0], method, inputSchema: true, outputSchema: false,
})), serverRequests: [], serverNotifications: [], clientNotifications: [], derivedLegacyTypes: []});
const captured = action => {
  try { return {ok: true, value: action()}; }
  catch (error) { return {ok: false, error: IvyError.from(error).toWire()}; }
};
const registryRows = [];
const definitionsByCatalog = new Map();
function nativeDefinitions(catalog) {
  const hash = hashJson(catalog);
  let definitions = definitionsByCatalog.get(hash);
  if (!definitions) {
    definitions = agentRegistry(catalog).namespaces[0].tools;
    definitionsByCatalog.set(hash, definitions);
  }
  return definitions;
}
function registry(label, reference) {
  const catalog = reference.path ? read(reference.path) : JSON.parse(JSON.stringify(reference.catalog));
  const expected = captured(() => {
    const value = agentRegistry(catalog);
    definitionsByCatalog.set(hashJson(catalog), value.namespaces[0].tools);
    return {hash: hashJson(value), definitions: value.namespaces.map(namespace => ({
      namespace: namespace.namespace,
      definitions: namespace.tools.map(tool => ({name: tool.name, hash: toolDefinitionHash(tool)})),
    }))};
  });
  registryRows.push({label, ...reference, expected});
}
for (const path of paths) registry(path, {path});
registry('empty-native', {catalog: minimal([])});
const names = [
  'thread/start', 'thread/list', 'thread/read', 'thread/resume', 'thread/fork',
  'thread/archive', 'thread/unarchive', 'thread/realtime/start', 'thread/realtime/stop',
  'thread/READExtra', 'thread/items', 'thread/turns', 'thread/history', 'thread/messages',
  'thread/loaded/list', 'thread/output', 'thread/name/set', 'thread/METADATA', 'thread/goal',
  'thread/settings', 'thread/configuration', 'thread/set', 'thread/ſet', 'thread/İtems',
  'thread/REALTIME/start', 'thread', 'thread/', 'project/list', 'project/create', 'threadSection/list',
  'turn/start', 'turn/steer', 'turn/interrupt', 'account/rateLimits/read', 'model/list',
  'modelProvider/list', 'getAuthStatus', 'config/read', 'configRequirements/read',
  'environment/list', 'permissionProfile/list', 'externalAgentConfig/read', 'windowsSandbox/setup',
  'experimentalFeature/list', 'ui/read', 'plugin/list', 'marketplace/list', 'skills/list',
  'mcpServer/read', 'mcpServerStatus/list', 'fs/readFile', 'command/exec', 'process/read',
  'fuzzyFileSearch', 'gitDiffToRemote', 'unknown/readXML', 'unknown/readXML', '/x',
  'projectX/list', 'Account/read', 'newHead/aBaB', 'newHead//last', 'é/😀A', '\ud800/aB',
  'thread/\udfff', 'ΟΣ/İ', '𐐀/read',
];
const opaque = minimal(names);
opaque.clientRequests[0].inputSchema = {$agentSchema: 'Status', nested: {$browserNoticeTopic: 'agent'}, '\ud800': '\udfff'};
opaque.clientRequests[0].outputSchema = {type: 'object', properties: {['__proto__']: {type: 'string'}}};
registry('group-order-duplicates-utf16-opaque-schemas', {catalog: opaque});
for (const method of Object.getOwnPropertyNames(Object.prototype)) registry('inherited-summary-' + method, {catalog: minimal([method])});
for (const [label, catalog] of [
  ['null', null], ['array', []], ['missing', {}],
  ['provider', {...minimal([]), provider: 'other'}],
  ['method-empty', minimal([''])], ['method-too-long', minimal(['x'.repeat(193)])],
  ['missing-native-executable', {...minimal([]), nativeExecutableHash: undefined}],
  ['invalid-source-hash', {...minimal([]), sourceHash: 'bad'}],
  ['additional-property', {...minimal([]), extra: true}],
]) registry('invalid-catalog-' + label, {catalog});
assert(registryRows.slice(0, 7).every(row => row.expected.ok));
assert(registryRows.find(row => row.label === 'group-order-duplicates-utf16-opaque-schemas').expected.ok);
for (const row of registryRows.slice(0, 7)) assert.equal(row.expected.value.definitions[1].definitions.length, 22);

const lowercase = new Map();
function remember(value) {lowercase.set(value, value.toLocaleLowerCase('en'));}
const discoveryRows = [];
function discovery(label, reference, input, platform = 'win32', overrides) {
  const catalog = reference.path ? read(reference.path) : reference.catalog;
  const definitions = overrides ?? nativeDefinitions(catalog);
  if (typeof input?.query === 'string') remember(input.query);
  for (const definition of definitions) remember([
    definition.name, definition.description, definition.discovery?.summary, definition.discovery?.group,
  ].join(' '));
  const expected = captured(() => {
    const page = discoverNativeTools(catalog, definitions, input, platform);
    return {hash: hashJson(page), count: page.items.length};
  });
  discoveryRows.push({label, ...reference, input, platform, ...(overrides ? {definitions: overrides} : {}), expected});
}
for (const path of paths) {
  for (const query of [undefined, '', 'CODEX', 'read task', 'task read', 'realtime voice', 'account USAGE', 'absent-uniquely-7654']) {
    discovery(path + ':' + String(query), {path}, query === undefined ? {} : {query});
  }
  for (const method of ['thread/read', 'thread/start', 'command/exec']) {
    for (const platform of ['win32', 'linux', 'darwin']) discovery(path + ':' + method + ':' + platform, {path}, {method}, platform);
  }
}
const synthetic = minimal(['command/exec', 'ΟΣ/read', 'İ/read', '𐐀/READ', '\ud800/read', 'read/\udfff']);
const definitions = structuredClone(nativeDefinitions(synthetic));
definitions[1].discovery.summary = 'ΟΣ AΣ́ AΣ\ud800 ΣA';
definitions[2].description = 'İ I ı i\u0307';
definitions[3].discovery.summary = '𐐀𐐁 𐐀A𐐀';
const spaces = '\u0009\u000a\u000b\u000c\u000d\u0020\u00a0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000\ufeff';
for (const query of [spaces, 'READ' + spaces + 'CODEX', 'read read', 'read\u0085codex', 'read\u180ecodex', 'read\u200bcodex',
  'ος', 'οσ', 'aς́', 'aς\ud800', 'σa', 'i\u0307', 'İ', 'ı', '𐐨𐐩', '𐐀', '\ud800', '\udfff', 'CODEX/ΟΣ']) {
  discovery('unicode-search-' + discoveryRows.length, {catalog: synthetic}, {query}, 'win32', definitions);
}
for (const input of [null, [], true, {query: null}, {query: 5}, {query: 'x'.repeat(513)}, {unknown: true},
  {query: '', method: 'command/exec'}, {method: ''}, {method: 'absent'}, {method: '\ud800/read'}]) {
  discovery('invalid-or-exact-input-' + discoveryRows.length, {catalog: synthetic}, input, 'win32', definitions);
}
const duplicate = [definitions[0], definitions[0]];
discovery('duplicate-exact', {catalog: synthetic}, {method: 'command/exec'}, 'win32', duplicate);
discovery('duplicate-query', {catalog: synthetic}, {}, 'win32', duplicate);
const plain = definitions.map(definition => {const copy = structuredClone(definition); delete copy.discovery; return copy;});
discovery('no-discovery', {catalog: synthetic}, {query: 'CODEx'}, 'win32', plain);
discovery('no-discovery-exact', {catalog: synthetic}, {method: 'command/exec'}, 'linux', plain);
const modified = structuredClone(definitions[0]); modified.discovery = {group: 'different', summary: 'different'};
assert.equal(toolDefinitionHash(modified), toolDefinitionHash(definitions[0]));
assert(discoveryRows.some(row => row.expected.ok && row.expected.value.count === 0));
assert(discoveryRows.some(row => !row.expected.ok && row.expected.error.data.code === 'native_method_unsupported'));
const schemas = ['StatusInput', 'Status', 'Notification', 'ThreadSummary', 'ProjectSummary', 'InstructionsDocument', 'McpDocument', 'SkillsDocument']
  .map(name => ({name, hash: hashJson(agentSchema(name))}));
process.stdout.write(JSON.stringify({nodeVersion: process.versions.node, registry: registryRows, discovery: discoveryRows,
  schemas, lowercase: [...lowercase]}));
