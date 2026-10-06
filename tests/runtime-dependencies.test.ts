import test from 'node:test';
import assert from 'node:assert/strict';
import { developmentPackageLocations, runtimePackageLocations, workspacePackageLocations } from '../packages/host-runtime/src/runtime-dependencies.js';

test('runtime package selection excludes exact dev-only locations while retaining shared and optional runtime dependencies', () => {
  const excluded = developmentPackageLocations({ lockfileVersion: 3, packages: {
    '': {}, 'node_modules/dev-tool': { dev: true }, 'node_modules/@scope/dev-tool': { dev: true },
    'node_modules/runtime/node_modules/dev-tool': { dev: true }, 'node_modules/shared': {},
    'node_modules/optional-shared': { devOptional: true }, 'node_modules/optional-runtime': { optional: true },
    'node_modules/dev-platform-helper': { dev: true, optional: true },
    'node_modules/workspace': { link: true, resolved: 'packages/workspace' }, 'packages/workspace': {},
  } });
  assert.deepEqual([...excluded].sort(), ['node_modules/@scope/dev-tool', 'node_modules/dev-platform-helper', 'node_modules/dev-tool', 'node_modules/runtime/node_modules/dev-tool']);
  assert.equal(excluded.has('node_modules/dev-tool-extra'), false);
  assert.equal(excluded.has('node_modules/optional-shared'), false);
});

test('runtime dependency cache identifies exact workspace links without expanding their current source bytes', () => {
  const links = workspacePackageLocations({ lockfileVersion: 3, packages: {
    '': {}, 'node_modules/@ivy/ui': { link: true, resolved: 'packages/ui' }, 'packages/ui': {},
    'node_modules/runtime': {},
  } });
  assert.deepEqual([...links.entries()], [['node_modules/@ivy/ui', 'packages/ui']]);
});

test('runtime package closure follows exact hoisted, nested, optional and peer dependency locations', () => {
  const selected = runtimePackageLocations({ lockfileVersion: 3, packages: {
    '': {},
    'node_modules/runtime': { dependencies: { shared: '1', nested: '1' }, optionalDependencies: { optional: '1', absent: '1' }, peerDependencies: { peer: '1' } },
    'node_modules/shared': { dependencies: { leaf: '1' } },
    'node_modules/leaf': {},
    'node_modules/runtime/node_modules/nested': { dependencies: { shared: '1' } },
    'node_modules/optional': {},
    'node_modules/peer': {},
    'node_modules/dev-only': { dev: true },
  } }, ['runtime']);
  assert.deepEqual([...selected].sort(), ['node_modules/leaf', 'node_modules/optional', 'node_modules/peer', 'node_modules/runtime',
    'node_modules/runtime/node_modules/nested', 'node_modules/shared']);
});

test('runtime package selection refuses unsupported or ambiguous lock metadata', () => {
  for (const lock of [null, { lockfileVersion: 2, packages: {} }, { lockfileVersion: 3 },
    { lockfileVersion: 3, packages: { 'node_modules/compiler': { dev: 'true' } } },
    { lockfileVersion: 3, packages: { 'node_modules/compiler': { dev: true, devOptional: true } } },
    ...['node_modules/../retained', 'node_modules/@scope/../../retained', 'node_modules/compiler/file.js'].map(location => ({ lockfileVersion: 3, packages: { [location]: { dev: true } } })),
  ]) assert.throws(() => developmentPackageLocations(lock), (error: unknown) => (error as { code: string }).code === 'invalid_arguments');
  for (const lock of [
    { lockfileVersion: 3, packages: { 'node_modules/workspace': { link: true, resolved: '../outside' } } },
    { lockfileVersion: 3, packages: { 'node_modules/workspace': { link: true, resolved: '' } } },
  ]) assert.throws(() => workspacePackageLocations(lock), (error: unknown) => (error as { code: string }).code === 'invalid_arguments');
});
