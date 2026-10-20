// Execute the original declarations with explicit path/account inputs. No service,
// provider, filesystem backup or executing-account home is opened by this oracle.
import ts from 'typescript';
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { win32, posix } from 'node:path';

const selections = [
  ['packages/contracts/src/errors.ts', ['statuses', 'IvyError', 'fail', 'requireThat']],
  ['packages/host-runtime/src/codex-home.ts', ['usesExternalAppServer', 'expectedServerHome', 'resolveCodexHome', 'instanceOwnedCodexHome']],
  ['packages/host-runtime/src/layout.ts', ['accountHome', 'servicePaths', 'hostStorageAreas', 'storagePath']],
  ['packages/host-runtime/src/config.ts', ['inside']],
  ['packages/host-runtime/src/native-backup-paths.ts', ['nativeBackupExclusions', 'sharedNativeHomes']],
];
const sources = [], declarations = [];
for (const [path, names] of selections) {
  const source = readFileSync(path, 'utf8');
  sources.push({ path, sha256: createHash('sha256').update(source).digest('hex'), names });
  const parsed = ts.createSourceFile(path, source, ts.ScriptTarget.Latest, true);
  const found = [];
  for (const node of parsed.statements) {
    const name = node.name?.text ?? (ts.isVariableStatement(node) && node.declarationList.declarations.length === 1
      ? node.declarationList.declarations[0].name.getText(parsed) : undefined);
    if (names.includes(name)) {
      found.push(name);
      declarations.push(node.getText(parsed).replace(/^export\s+/, ''));
    }
  }
  if (found.length !== names.length || names.some(name => !found.includes(name))) throw Error('Incomplete original declarations: ' + path);
}
const body = ts.transpileModule(declarations.join('\n'), {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
}).outputText + '\nreturn {nativeBackupExclusions,sharedNativeHomes,IvyError,accountHome};';
const factory = new Function('process', 'homedir', 'win32', 'posix', 'join', 'isAbsolute', 'relative', 'resolve', body);
const context = {
  processCwd: process.cwd(), lexicalCwds: { win32: win32.resolve(), linux: posix.resolve() },
  driveCwds: Object.fromEntries(Object.entries(process.env).filter(([key]) => /^=[a-z]:$/i.test(key))),
  nativePlatform: process.platform, node: process.version, icu: process.versions.icu,
  locale: Intl.DateTimeFormat().resolvedOptions().locale, typescript: ts.version,
};
const rows = [], words = new Set(['']);
for (const [platform, paths] of [['win32', win32], ['linux', posix]]) {
  const root = platform === 'win32' ? 'C:\\fixture' : '/fixture';
  const path = (...parts) => paths.join(root, ...parts);
  const accountHome = path('account');
  const base = (modern = false) => ({
    runtimeRoot: path('runtime'), artifactRoot: path('software'), stagingRoot: path('staging'),
    ...(modern ? { servicesRoot: path('services') } : {}), instances: [],
  });
  const agent = (id, settings = {}, extra = {}) => ({ instanceId: id, componentId: 'agent-manager', settings, ...extra });
  const data = (id = 'agent', modern = false) => modern ? path('services', 'agent-manager', 'data') : path('runtime', 'instances', id, 'data');
  const owned = (id = 'agent', modern = false, tail = 'codex-home') => ({ appServer: { mode: 'owned-stdio' }, codexHome: paths.join(data(id, modern), tail) });
  const external = home => ({ appServer: { mode: 'external-proxy', expectedCodexHome: home } });
  const add = (group, name, config, environment = {}) => {
    const fakeProcess = Object.freeze({ platform, env: Object.freeze(environment), cwd: () => context.processCwd });
    const original = factory(fakeProcess, () => accountHome, win32, posix, paths.join, paths.isAbsolute, paths.relative, paths.resolve);
    const capture = fn => { try { return { result: fn() }; } catch (error) { return { error: original.IvyError.from(error).toWire() }; } };
    const expected = {
      exclusions: capture(() => original.nativeBackupExclusions(config)),
      sharedHomes: capture(() => original.sharedNativeHomes(config)),
    };
    for (const instance of Array.isArray(config.instances) ? config.instances : []) if (typeof instance?.instanceId === 'string') words.add(instance.instanceId);
    for (const outcome of Object.values(expected)) for (const row of outcome.result ?? []) if (typeof row.path === 'string') words.add(row.path);
    rows.push({ platform, group, name, config, environment, accountHome: original.accountHome(), expected });
  };
  const single = (settings, extra = {}, modern = false) => ({ ...base(modern), instances: [agent('agent', settings, extra)] });

  add('ownership', 'empty-host', base());
  add('ownership', 'ignore-other-components-with-malformed-settings', { ...base(), instances: [{ instanceId: 'other', componentId: 'other', settings: null, paths: false }] });
  for (const modern of [false, true]) {
    add('ownership', `explicit-owned-${modern}`, single(owned('agent', modern), {}, modern));
    add('ownership', `claude-owned-${modern}`, single({ ...owned('agent', modern), appServer: { mode: 'claude-adapter' } }, {}, modern));
    add('ownership', `native-home-alias-${modern}`, single({ appServer: { mode: 'owned-stdio' }, nativeHome: owned('agent', modern).codexHome }, {}, modern));
    add('ownership', `normalized-descendant-${modern}`, single({ ...owned('agent', modern), codexHome: paths.join(data('agent', modern), 'folder', '..', 'home') + paths.sep }, {}, modern));
    add('ownership', `equal-root-not-owned-${modern}`, single({ ...owned('agent', modern), codexHome: data('agent', modern) }, {}, modern));
    add('ownership', `parent-root-not-owned-${modern}`, single({ ...owned('agent', modern), codexHome: paths.dirname(data('agent', modern)) }, {}, modern));
    add('ownership', `sibling-prefix-not-owned-${modern}`, single({ ...owned('agent', modern), codexHome: data('agent', modern) + '-sibling' }, {}, modern));
    add('ownership', `dotdot-prefix-descendant-${modern}`, single(owned('agent', modern, '..cache'), {}, modern));
    add('ownership', `no-explicit-env-under-data-not-owned-${modern}`, single({ appServer: { mode: 'owned-stdio' } }, {}, modern), { CODEX_HOME: paths.join(data('agent', modern), 'implicit') });
    const override = { data: path('custom-data'), work: path('custom-work'), logs: path('custom-logs') };
    add('ownership', `override-service-paths-${modern}`, single({ ...owned('agent', modern), codexHome: paths.join(override.data, 'home') }, { paths: override }, modern));
  }
  add('ownership', 'owned-skips-unrelated-host-area-errors', { ...base(), instances: [agent('agent', owned()), { instanceId: 'other', componentId: 'other', settings: {}, paths: { data: false } }] });
  add('ownership', 'non-agent-home-does-not-become-exclusion', { ...base(), instances: [{ ...agent('other', owned('other')), componentId: 'other' }] });
  add('ownership', 'external-mode-never-owns-even-explicit-descendant', single({ ...owned(), ...external(owned().codexHome) }));
  add('ownership', 'missing-mode-never-owns-explicit-descendant', single({ codexHome: owned().codexHome }));
  add('ownership', 'duplicate-owned-paths-are-retained', { ...base(true), instances: ['first', 'second'].map(id => agent(id, owned(id, true))) });

  for (const [name, settings, environment] of [
    ['default-home', {}, {}],
    ['owned-mode-default-remains-shared', { appServer: { mode: 'owned-stdio' } }, {}],
    ['claude-mode-default-remains-shared', { appServer: { mode: 'claude-adapter' } }, {}],
    ['external-expected-home', external(path('shared')), {}],
    ['external-normalization-is-not-applied', external(path('shared') + paths.sep + '.' + paths.sep), {}],
    ['external-does-not-resolve-conflicting-explicit-settings', { ...external(path('shared')), codexHome: 'relative', nativeHome: false }, {}],
    ['unknown-mode', { appServer: { mode: 'unknown' }, codexHome: path('shared') }, {}],
    ['environment-home', {}, { CODEX_HOME: path('environment-home') }],
    ['trim-environment-home', {}, { CODEX_HOME: ' \t' + path('environment-home') + '\n' }],
    ['relative-environment-home', {}, { CODEX_HOME: 'relative-home' }],
    ['empty-environment-home', {}, { CODEX_HOME: '' }],
    ['whitespace-environment-home', {}, { CODEX_HOME: ' \t\n' }],
    ['profile-home', {}, { [platform === 'win32' ? 'USERPROFILE' : 'HOME']: path('profile') }],
  ]) add('shared', name, single(settings), environment);
  for (const [name, selected] of [
    ['runtime', path('runtime')], ['service', data()], ['projects', paths.join(accountHome, '.ivy', 'codex-projects')],
    ['artifact', path('software')], ['staging', path('staging')], ['custom-service', path('outside-data')], ['custom-project', path('outside-projects')],
  ]) {
    const config = single({}, { paths: { data: path('outside-data'), work: path('outside-work'), logs: path('outside-logs') } });
    config.instances[0].settings.internalProjectRoot = path('outside-projects');
    for (const [relation, home] of [['equal', selected], ['child', paths.join(selected, 'child')], ['parent', paths.dirname(selected)]]) {
      const copy = structuredClone(config); copy.instances[0].settings.appServer = external(home).appServer;
      add('shared', `${name}-${relation}-conflict`, copy);
    }
  }
  add('shared', 'sibling-with-owned-prefix-is-allowed', single(external(path('runtime') + '-sibling')));
  add('shared', 'projects-empty-string-uses-cwd', single({ ...external(context.lexicalCwds[platform]), internalProjectRoot: '' }));
  add('shared', 'projects-nonstrings-select-account-default', single({ ...external(paths.join(accountHome, '.ivy', 'codex-projects')), internalProjectRoot: false }));
  add('shared', 'runtime-conflict-short-circuits-invalid-artifact', { ...single(external(path('runtime'))), artifactRoot: false });
  add('shared', 'artifact-conflict-short-circuits-invalid-staging', { ...single(external(path('software'))), stagingRoot: false });
  add('shared', 'disjoint-home-reaches-invalid-artifact', { ...single(external(path('shared'))), artifactRoot: false });
  if (platform === 'win32') {
    add('shared', 'windows-case-insensitive-overlap', single(external(path('runtime').toUpperCase() + '\\child')));
    add('shared', 'different-drive-is-disjoint', single(external('D:\\shared-home')));
    add('shared', 'unc-home-is-disjoint', single(external('\\\\server\\share\\home')));
    add('shared', 'drive-relative-is-not-absolute', single(external('C:relative')));
  } else {
    add('shared', 'posix-case-sensitive-disjoint', single(external(path('runtime').toUpperCase())));
    add('shared', 'backslash-dotdot-name-still-checks-original-inside', single(external(path('runtime', '..\\outside'))));
  }

  for (const value of [null, false, true, 0, 1, [], {}, '', 'relative', '  ']) {
    add('errors', 'invalid-external-expected-' + JSON.stringify(value), { ...single(external(value)), runtimeRoot: false });
    for (const key of ['codexHome', 'nativeHome']) {
      add('settings', key + '-' + JSON.stringify(value), single({ appServer: { mode: 'owned-stdio' }, [key]: value }));
    }
  }
  for (const value of [null, false, 2, 'text', [], {}]) add('settings', 'raw-settings-' + JSON.stringify(value), single(value));
  add('settings', 'null-codex-selects-native', single({ appServer: { mode: 'owned-stdio' }, codexHome: null, nativeHome: owned().codexHome }));
  add('settings', 'two-homes-conflict', single({ ...owned(), nativeHome: path('other-home') }));
  add('settings', 'two-homes-normalized-equal', single({ ...owned(), nativeHome: paths.join(data(), '.', 'codex-home') }));
  for (const [name, override] of [['missing', {}], ['boolean', { data: false }], ['number', { data: 4 }]]) {
    add('errors', `no-explicit-home-skips-${name}-data`, single({ appServer: { mode: 'owned-stdio' } }, { paths: override }));
    add('errors', `explicit-home-validates-${name}-data`, single(owned(), { paths: override }));
    add('errors', `invalid-home-precedes-${name}-data`, single({ ...owned(), codexHome: 'relative' }, { paths: override }));
  }
  add('errors', 'service-base-evaluated-before-explicit-presence', { ...single({ appServer: { mode: 'owned-stdio' } }, { paths: {} }), runtimeRoot: false });
  add('errors', 'external-skips-service-base', { ...single(external(false), { paths: {} }), runtimeRoot: false });
  add('errors', 'filter-completes-before-invalid-home', { ...base(), instances: [agent('agent', { ...owned(), codexHome: 'relative' }), null] });
  add('errors', 'missing-agent-settings', { ...base(), instances: [{ instanceId: 'agent', componentId: 'agent-manager' }] });

  const ids = ['z', 'Z', 'a', 'A', 'ä', 'a\u0308', 'é', 'e\u0301', '2', '10', '😀', '中', 'duplicate', 'duplicate'];
  add('sort', 'shared-ids-use-default-locale-stable-ties', { ...base(), instances: ids.map((id, i) => agent(id, external(path('shared-' + i)))) });
  add('sort', 'exclusions-sort-storage-paths-not-ids', { ...base(), instances: ids.map((id, i) => {
    const customData = path('custom', ids[ids.length - i - 1] + '-' + i);
    return agent(id, { appServer: { mode: 'owned-stdio' }, codexHome: paths.join(customData, 'home') }, { paths: { data: customData, work: paths.join(customData, 'work'), logs: paths.join(customData, 'logs') } });
  }) });
}
// Supply every comparator pair explicitly. Rust must fail on an unqualified key;
// this table is a test input, never a production ordering fallback.
const comparisons = [];
for (const left of words) for (const right of words) comparisons.push([left, right, Math.sign(left.localeCompare(right))]);
process.stdout.write(JSON.stringify({ context, sources, comparisons, rows }));
