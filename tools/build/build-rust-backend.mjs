import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, realpathSync, statSync, writeFileSync } from 'node:fs';
import { isAbsolute, relative, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { buildIdentity } from '../../packages/host-runtime/src/build-identity.mjs';
import { buildInstructions } from './build-instructions.mjs';

const targets = new Map([
  ['x86_64-pc-windows-msvc', 'win32-x64'],
  ['aarch64-pc-windows-msvc', 'win32-arm64'],
  ['x86_64-unknown-linux-gnu', 'linux-x64'],
  ['aarch64-unknown-linux-gnu', 'linux-arm64'],
]);
function within(root, path) {
  const child = relative(root, path);
  return child !== '' && !isAbsolute(child) && child !== '..' && !child.startsWith('..' + (process.platform === 'win32' ? '\\' : '/'));
}
function runCargo(args, options) {
  const result = spawnSync(process.env.IVY_CARGO ?? 'cargo', args, {
    ...options, encoding: 'utf8', maxBuffer: 16 * 1024 * 1024, windowsHide: true,
  });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(result.stderr || `Cargo exited with ${result.status}.`);
  return result.stdout;
}

/** Compile the selected component's real binaries into an isolated target payload.
 * This is a builder input to universal package assembly, not a release archive or
 * proof that other advertised targets and host readiness helpers are available. */
export function compileRustBackend({ component, target, root = process.cwd(), identity, cargo = runCargo } = {}) {
  if (!/^[a-z][a-z0-9-]*$/.test(component ?? '')) throw new Error('A component name is required.');
  const platform = targets.get(target);
  if (!platform) throw new Error('An explicit supported Rust target is required.');
  root = realpathSync(root);
  const manifest = resolve(root, component === 'host-executor' ? 'packages/host-runtime/rust/Cargo.toml' : `services/${component}/rust/Cargo.toml`);
  if (!existsSync(manifest) || realpathSync(manifest) !== manifest) throw new Error('The component Rust manifest must exist without directory links.');
  const targetDirectory = resolve(root, '.local', 'cargo', component);
  const releaseDirectory = resolve(targetDirectory, target, 'release');
  const outputDirectory = resolve(root, '.local', 'build', 'rust', component, platform);
  identity ??= buildIdentity(root);
  const options = { cwd: root, env: { ...process.env, IVY_BUILD_ID: identity.buildId, CARGO_TARGET_DIR: targetDirectory, CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS ?? '1' } };
  const metadata = JSON.parse(cargo(['metadata', '--locked', '--no-deps', '--format-version', '1', '--manifest-path', manifest], options));
  const selected = metadata.packages.find(pkg => resolve(pkg.manifest_path) === manifest);
  if (!selected || !metadata.workspace_members.includes(selected.id)) throw new Error('Register the delivered component in the Rust workspace before building it.');
  const binaries = selected.targets.filter(t => t.kind.includes('bin'));
  if (!binaries.some(t => t.name === `ivy-${component}`)) throw new Error('The component has no native service executable; a library is not a backend.');
  for (const binary of binaries) if (!/^[A-Za-z0-9_-]+$/.test(binary.name)) throw new Error('Invalid native executable name.');
  const features = [...new Set(binaries.flatMap(t => t['required-features'] ?? []))];
  // Explicit required features also include optional binary targets; default
  // features remain enabled (for example the selected component's browser).
  const args = ['build', '--locked', '--release', '--target', target, '--package', selected.name,
    ...binaries.flatMap(t => ['--bin', t.name]), ...(features.length ? ['--features', features.join(',')] : []), '--message-format=json-render-diagnostics'];
  const messages = cargo(args, options).split(/\r?\n/).filter(Boolean).map(line => JSON.parse(line));
  const artifacts = new Map();
  for (const message of messages) {
    if (message.reason !== 'compiler-artifact' || message.package_id !== selected.id || !message.executable || !message.target.kind.includes('bin')) continue;
    const source = resolve(message.executable);
    if (!within(releaseDirectory, source) || realpathSync(source) !== source || !statSync(source).isFile()) throw new Error('Cargo executable escaped its selected target release directory.');
    artifacts.set(message.target.name, source);
  }
  // Validate the complete set before copying; never substitute a previous file
  // when Cargo omitted a requested executable or failed the selected build.
  for (const binary of binaries) if (!artifacts.has(binary.name)) throw new Error(`Cargo did not produce ${binary.name}.`);
  mkdirSync(outputDirectory, { recursive: true });
  if (realpathSync(outputDirectory) !== outputDirectory) throw new Error('Native output must not use directory links.');
  const files = [];
  for (const binary of binaries) {
    const name = binary.name + (platform.startsWith('win32-') ? '.exe' : '');
    const destination = resolve(outputDirectory, name);
    if (existsSync(destination) && realpathSync(destination) !== destination) throw new Error('Native output must not use file links.');
    copyFileSync(artifacts.get(binary.name), destination);
    files.push(destination);
  }
  const buildInfo = resolve(outputDirectory, 'build-info.json');
  if (existsSync(buildInfo) && realpathSync(buildInfo) !== buildInfo) throw new Error('Native build identity must not use file links.');
  writeFileSync(buildInfo, JSON.stringify(identity, null, 2) + '\n');
  const instructionFiles = buildInstructions({ root, component, outputRoot: resolve(root, '.local', 'build', 'rust', component) });
  return { component, target, platform, identity, outputDirectory, files, instructionFiles };
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const value = name => process.argv[process.argv.indexOf(name) + 1];
  if (!process.argv.includes('--component') || !process.argv.includes('--target')) throw new Error('Use --component <name> --target <triple>.');
  compileRustBackend({ component: value('--component'), target: value('--target') });
}
