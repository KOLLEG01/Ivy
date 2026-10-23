import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, isAbsolute, relative, resolve } from 'node:path';
import { rustBackendTargets } from './build-rust-backend.mjs';
import { materializeNativePlan } from '../../packages/host-runtime/src/native-plan.mjs';

function within(root, path) {
  const child = relative(root, path);
  return child !== '' && !isAbsolute(child) && child !== '..' && !child.startsWith('..' + (process.platform === 'win32' ? '\\' : '/'));
}
function regular(root, path) {
  path = resolve(path);
  if (!within(root, path) || realpathSync(path) !== path || !statSync(path).isFile()) throw Error('Native input must be a contained regular file without links.');
  return path;
}
function sameIdentity(actual, expected) {
  return actual?.schemaVersion === 1 && actual.version === expected.version && actual.buildId === expected.buildId;
}
function choices(value, fallback, allowed) {
  if (value === undefined) value = fallback;
  if (!Array.isArray(value) || !value.length || new Set(value).size !== value.length || value.some(v => !allowed.includes(v))) throw Error('Invalid advertised native platform matrix.');
  return value;
}

function runtimeAssets(component, root, os) {
  if (component === 'host-executor' && os.includes('win32')) {
    return ['install-windows.ps1', 'bootstrap-maintenance-windows.ps1', 'windows-principal.ps1'].map(name => {
      const path = 'packages/host-runtime/assets/' + name;
      return { path, bytes: readFileSync(regular(root, resolve(root, path))) };
    });
  }
  if (!['agent-manager', 'service-manager', 'chat-bridge', 'task-board', 'phone-bridge', 'secretary'].includes(component)) return [];
  const assets = [];
  const visit = directory => {
    if (realpathSync(directory) !== directory) throw Error('Native catalog inputs must not use links.');
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const source = resolve(directory, entry.name);
      if (entry.isDirectory()) visit(source);
      else if (entry.name.endsWith('.json') || entry.name === 'LICENSE.codex') {
        assets.push({ path: relative(root, source), bytes: readFileSync(regular(root, source)) });
      } else if (entry.isSymbolicLink()) throw Error('Native catalog inputs must not use links.');
    }
  };
  visit(resolve(root, 'specs/native'));
  if (!assets.some(asset => /(?:^|[\\/])catalog(?:\.[^\\/]*)?\.json$/.test(asset.path))) throw Error('Native runtime catalogs are missing.');
  if (component === 'agent-manager') {
    for (const name of ['hive-mcp-instructions.md', 'hive-mcp-agent-summary.md', 'hive-mcp-examples.md', 'ivy-dev-mcp-instructions.md'])
      assets.push({ path: 'instructions/' + name, bytes: readFileSync(regular(root, resolve(root, 'instructions', name))) });
  }
  // Intentional JavaScript workers remain explicit runtime assets.
  // The existing preparation path resolves its literal PDF.js import through
  // the pinned npm lock and materializes all supported platform dependencies.
  if (component === 'secretary') assets.push({ path: 'services/secretary/pdf-worker.mjs',
    bytes: readFileSync(regular(root, resolve(root, 'services/secretary/rust/pdf-worker.mjs'))) });
  return assets;
}

/** Assemble records from compileRustBackend and compileRustHelpers for a prepared
 * host plan. This does not compile, qualify or archive another source target.
 * Callers declare extra required helper names by OS. Linux executable paths are
 * also returned for an archive writer assembling on a filesystem without Unix modes. */
export function stageNativePayloads({ component, root = process.cwd(), outputRoot = resolve(root, 'dist'), identity, payloads, requiredBinaries = {}, plan } = {}) {
  root = realpathSync(root);
  if (!/^[a-z][a-z0-9-]*$/.test(component ?? '')) throw Error('A component name is required.');
  plan ??= materializeNativePlan(JSON.parse(readFileSync(resolve(root, 'services', component, 'deploy.json'), 'utf8')));
  if (plan.schemaVersion !== 1 || typeof plan.version !== 'string' || !plan.version || typeof plan.requirements?.node !== 'string' || !plan.requirements.node) throw Error('Native staging requires the unchanged v1 plan and Node requirement.');
  if (!identity || identity.schemaVersion !== 1 || typeof identity.version !== 'string' || !identity.version || !/^sha256:[0-9a-f]{64}$/.test(identity.buildId)) throw Error('A shared native build identity is required.');
  const os = choices(plan.requirements.os, ['win32', 'linux'], ['win32', 'linux']);
  const arch = choices(plan.requirements.arch, ['x64', 'arm64'], ['x64', 'arm64']);
  const platforms = Object.values(rustBackendTargets).filter(p => { const [o, a] = p.split('-'); return os.includes(o) && arch.includes(a); });
  if (platforms.length !== 1) throw Error('Native staging requires one materialized host plan.');
  const suffix = os[0] === 'win32' ? '.exe' : '';
  if (plan.entrypoint?.executable !== `dist/native/${platforms[0]}/ivy-${component}${suffix}` ||
      plan.readiness?.command?.executable !== `dist/native/${platforms[0]}/ivy-health${suffix}`)
    throw Error('Native staging requires direct native service and health commands.');
  if (!Array.isArray(payloads)) throw Error('Explicit prebuilt payload records are required.');
  if (!requiredBinaries || Array.isArray(requiredBinaries) || typeof requiredBinaries !== 'object' || Object.keys(requiredBinaries).some(key => !['win32', 'linux'].includes(key))) throw Error('Required native helpers must be declared by OS.');
  for (const names of Object.values(requiredBinaries)) if (!Array.isArray(names) || names.some(name => typeof name !== 'string' || !/^[A-Za-z0-9_-]+$/.test(name))) throw Error('Invalid required native helper name.');
  const selected = new Map(platforms.map(platform => [platform, { primary: 0, files: new Map(), instructions: null }]));
  const seen = new Set();
  for (const payload of payloads) {
    const platform = typeof payload?.target === 'string' && Object.hasOwn(rustBackendTargets, payload.target) ? rustBackendTargets[payload.target] : undefined;
    if (!platform || platform !== payload.platform || !selected.has(platform)) throw Error('Native payload does not match the advertised target.');
    if (typeof payload.component !== 'string' || !/^[a-z][a-z0-9-]*$/.test(payload.component)) throw Error('Invalid native payload component.');
    const key = platform + '/' + payload.component;
    if (seen.has(key)) throw Error('Duplicate native component target.');
    seen.add(key);
    if (!sameIdentity(payload.identity, identity)) throw Error('Native payload build identity differs.');
    const input = resolve(payload.outputDirectory);
    if (!within(root, input) || realpathSync(input) !== input || !statSync(input).isDirectory()) throw Error('Native payload directory must be contained without links.');
    const info = JSON.parse(readFileSync(regular(input, resolve(input, 'build-info.json')), 'utf8'));
    if (!sameIdentity(info, identity)) throw Error('Native payload build stamp differs.');
    if (!Array.isArray(payload.files) || !payload.files.length) throw Error('Native payload has no executable files.');
    const group = selected.get(platform), windows = platform.startsWith('win32-');
    for (const file of payload.files) {
      const source = regular(input, file), name = basename(source);
      if (dirname(source) !== input || !(windows ? /^[A-Za-z0-9_-]+\.exe$/ : /^[A-Za-z0-9_-]+$/).test(name) || statSync(source).size === 0) throw Error('Invalid selected native executable.');
      const folded = name.toLowerCase();
      if (group.files.has(folded)) throw Error('Native executable name collision.');
      group.files.set(folded, { source, name });
    }
    if (!Array.isArray(payload.runtimeFiles ?? [])) throw Error('Native runtime files must be an array.');
    for (const file of payload.runtimeFiles ?? []) {
      const source = regular(input, file), name = basename(source);
      if (dirname(source) !== input || statSync(source).size === 0 ||
          (!(windows ? /^[A-Za-z0-9_-]+\.dll$/ : /^lib[A-Za-z0-9_-]+\.so(?:\.\d+)*$/).test(name) && !/^LICENSE\.[A-Za-z0-9_-]+$/.test(name))) throw Error('Invalid selected native runtime file.');
      const folded = name.toLowerCase(), existing = group.files.get(folded);
      // Multiple binaries in one target may use the same shared runtime. Keep
      // one exact copy; different bytes must not silently win by input order.
      if (existing) {
        if (!existing.runtime || existing.name !== name || !readFileSync(existing.source).equals(readFileSync(source))) throw Error('Native runtime name collision.');
      } else group.files.set(folded, { source, name, runtime: true });
    }
    if (payload.component === component) {
      group.primary++;
      if (!payload.files.some(file => basename(file) === 'ivy-' + component + (windows ? '.exe' : ''))) throw Error('Native component service executable is missing.');
      const instructions = new Map();
      for (const file of payload.instructionFiles ?? []) {
        const source = regular(root, file), name = basename(source);
        if (!/^[a-z][a-z0-9-]*\.json$/.test(name) || instructions.has(name)) throw Error('Invalid shared instruction asset.');
        instructions.set(name, { source, bytes: readFileSync(source) });
      }
      group.instructions = instructions;
    }
  }
  let instructions;
  for (const [platform, group] of selected) {
    if (group.primary !== 1) throw Error('The prepared host requires the selected component payload.');
    const windows = platform.startsWith('win32-');
    for (const name of ['ivy-' + component, 'ivy-health', ...(requiredBinaries[windows ? 'win32' : 'linux'] ?? [])]) {
      const file = group.files.get((name + (windows ? '.exe' : '')).toLowerCase());
      if (!file || file.name !== name + (windows ? '.exe' : '')) throw Error('Required native executable is missing: ' + platform + '/' + name);
    }
    instructions ??= group.instructions;
    if (instructions.size !== group.instructions.size || [...instructions].some(([name, value]) => !group.instructions.get(name)?.bytes.equals(value.bytes))) throw Error('Native targets have different shared instruction assets.');
  }
  const assets = runtimeAssets(component, root, os);
  outputRoot = resolve(outputRoot);
  let parent = outputRoot;
  while (!existsSync(parent)) parent = dirname(parent);
  if (realpathSync(parent) !== parent || !statSync(parent).isDirectory()) throw Error('Native staging output must not use links.');
  if (existsSync(outputRoot) && readdirSync(outputRoot).length) throw Error('Native staging requires a fresh empty output directory.');
  // All records are checked before writing an unpublished staging directory.
  const stamp = { schemaVersion: 1, componentId: component, version: plan.version, buildId: identity.buildId };
  const stampText = JSON.stringify(stamp, null, 2) + '\n';
  const files = [], linuxExecutables = [];
  for (const [platform, group] of selected) {
    const directory = resolve(outputRoot, 'native', platform);
    mkdirSync(directory, { recursive: true });
    for (const { source, name, runtime } of group.files.values()) {
      const path = resolve(directory, name);
      copyFileSync(source, path);
      if (platform.startsWith('linux-') && !runtime) {
        if (process.platform !== 'win32') chmodSync(path, 0o755);
        linuxExecutables.push(path);
      }
      files.push(path);
    }
    const info = resolve(directory, 'build-info.json');
    writeFileSync(info, stampText); files.push(info);
  }
  for (const [name, value] of instructions) {
    const path = resolve(outputRoot, 'instructions', name);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, value.bytes); files.push(path);
  }
  for (const asset of assets) {
    const path = resolve(outputRoot, asset.path);
    mkdirSync(dirname(path), { recursive: true });
    writeFileSync(path, asset.bytes); files.push(path);
  }
  const info = resolve(outputRoot, 'build-info.json');
  writeFileSync(info, stampText); files.push(info);
  return { component, identity, stamp, platforms, files, linuxExecutables };
}
