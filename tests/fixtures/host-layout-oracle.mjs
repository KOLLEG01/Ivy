import {existsSync} from 'node:fs';
import {registerHooks} from 'node:module';
import {fileURLToPath} from 'node:url';
import {homedir} from 'node:os';
import path from 'node:path';
registerHooks({resolve(specifier, context, next) {
  if (specifier.startsWith('.') && specifier.endsWith('.js')) {
    const source = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
    if (existsSync(fileURLToPath(source))) return next(source.href, context);
  }
  return next(specifier, context);
}});
const layout = await import('../../packages/host-runtime/src/layout.ts');
const {defaultServiceSettings} = await import('../../packages/host-runtime/src/default-settings.ts');
const {IvyError} = await import('../../packages/contracts/src/errors.ts');
const cases = [];
function add(style, operation, args) {
  let expected;
  try {
    let value;
    if (operation === 'agentManagerAccountSettings') value = layout[operation](...args, path[style].join);
    else if (operation === 'defaultServiceSettings') value = defaultServiceSettings(args[0], {...args[1], join: path[style].join});
    else value = layout[operation](...args);
    expected = {ok: true, value};
  } catch (error) { expected = {ok: false, error: IvyError.from(error).toWire()}; }
  cases.push({style, operation, args, osHome: homedir(), expected});
}
for (const style of ['win32', 'posix']) {
  const platform = style === 'win32' ? 'win32' : 'linux';
  const root = style === 'win32' ? 'C:\\fixture' : '/fixture';
  const home = path[style].join(root, 'account');
  const environment = {HOME: home, USERPROFILE: home};
  const source = path[style].join(root, 'host.json');
  for (const value of [undefined, '', '.', 'relative', ' ', '/', '//server/share/user', 'C:relative', 'C:/user/../account/', '\\user', home, home + '/\ud800']) {
    const env = {HOME: value, USERPROFILE: value};
    add(style, 'accountHome', [env, platform]);
    add(style, 'defaultHostConfigPath', [env, platform]);
  }
  const settingsCases = [{}, {codexHome: null}, {nativeHome: null}, {codexHome: ''}, {nativeHome: home}, {codexHome: home, nativeHome: '/different'},
    {codexHome: 5}, {codexHome: ['one', 'two']}, {codexHome: {x: 1}}, {codexHome: {toString: null}},
    {projectRoot: null, internalProjectRoot: null, skillsRoot: null}, {projectRoot: '', internalProjectRoot: false, skillsRoot: ''},
    {appServer: {mode: 'claude-adapter'}}, {appServer: {mode: 'claude-adapter'}, skillsRoot: null},
    {appServer: null}, {appServer: {mode: 'owned-stdio'}, codexHome: home + '/\ud800'},
    {capabilities: [], limits: {maxPendingInputs: 2}}, JSON.parse('{"__proto__":{"kept":true},"extension":"\\ud800"}')];
  for (const settings of settingsCases) add(style, 'agentManagerAccountSettings', [settings, home]);
  const instance = (componentId, settings = {}) => ({instanceId: componentId + '-renamed', componentId, engine: 'process', settings});
  const components = ['host-executor', 'service-manager', 'hive', 'agent-manager', 'task-board', 'chat-bridge', 'secretary', 'data-collector', 'dashboards', 'phone-bridge', 'extension'];
  const roots = [{}, {runtimeRoot: path[style].join(root, 'legacy')}, {ivyRoot: path[style].join(root, 'custom')},
    {runtimeRoot: path[style].join(root, 'runtime'), servicesRoot: path[style].join(root, 'services')},
    {runtimeRoot: null, ivyRoot: null, servicesRoot: null},
    {runtimeRoot: path[style].join(root, 'legacy'), configPath: path[style].join(root, 'explicit.json')},
    {artifactRoot: 'relative'}, {stagingRoot: ''}, {configPath: 'relative'}, {ivyRoot: 'relative'},
    {runtimeRoot: path[style].join(root, 'legacy'), packageUpdates: null, configurationUpdates: {intervalSeconds: 0}}];
  for (const rootsCase of roots) for (const sourcePath of [source, path[style].join(home, '.ivy', 'config.json'), path[style].join(home, '.ivy', 'child', '..', 'config.json')]) {
    add(style, 'resolveHostConfiguration', [{schemaVersion: 1, hostId: 'host', ...rootsCase, instances: components.map(c => instance(c))}, sourcePath, environment, platform]);
  }
  for (const settings of settingsCases) for (const paths of [undefined, null, {}, {data: null}, {data: path[style].join(root, 'data'), work: path[style].join(root, 'work'), logs: path[style].join(root, 'logs')}]) {
    add(style, 'resolveHostConfiguration', [{instances: [{...instance('agent-manager', settings), ...(paths === undefined ? {} : {paths})}]}, source, environment, platform]);
    add(style, 'resolveHostConfiguration', [{runtimeRoot: path[style].join(root, 'legacy'), instances: [{...instance('agent-manager', settings), ...(paths === undefined ? {} : {paths})}]}, source, environment, platform]);
  }
  for (const process of [undefined, null, false, 0, '', {}, {allowWindowsBreakaway: false}]) {
    add(style, 'resolveHostConfiguration', [{instances: [{...instance('service-manager'), ...(process === undefined ? {} : {process})}]}, source, environment, platform]);
  }
  for (const component of components) for (const settings of [{}, {backup: {directory: null}, extension: '\ud800'}]) {
    add(style, 'defaultServiceSettings', [instance(component, settings), {ivyRoot: root + '/\ud800', configPath: source + '\udc00'}]);
  }
}
const native = process.platform === 'win32' ? 'win32' : 'posix';
const root = native === 'win32' ? 'C:\\fixture' : '/fixture';
for (const servicesRoot of [undefined, null, '', path.join(root, 'services')]) for (const paths of [undefined, null, {}, {data: root + '/\ud800', work: root + '/work', logs: root + '/logs'}]) {
  const instance = {instanceId: 'renamed', componentId: 'extension', settings: {}, ...(paths === undefined ? {} : {paths})};
  const config = {runtimeRoot: path.join(root, 'runtime'), ...(servicesRoot === undefined ? {} : {servicesRoot}), instances: [instance]};
  add(native, 'servicePaths', [config, instance]);
  add(native, 'servicePaths', [config, 'renamed']);
  add(native, 'servicePaths', [config, 'missing']);
}
for (const configPath of [undefined, null, '', root + '/\ud800']) add(native, 'hostConfigurationPath', [{runtimeRoot: root, ...(configPath === undefined ? {} : {configPath})}]);
process.stdout.write(JSON.stringify(cases));
