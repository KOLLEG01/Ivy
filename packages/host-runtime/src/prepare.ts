import { candidateBuildId } from './candidate-identity.js';
import { PreparationProgress } from './preparation-progress.js';
import { mkdir, cp, stat, readFile, realpath, rename, copyFile, readdir, rm, symlink, link, chmod, writeFile } from 'node:fs/promises';
import type { Dirent } from 'node:fs';
import { createHash, randomUUID } from 'node:crypto';
import typescript from 'typescript';
import { join, resolve, relative, dirname } from 'node:path';
import { ExecutorLock, HostJournal } from './journal.js';
import { sourceBuildPlan, verifySnapshotMetadata } from './source.js';
import { atomicJson, jsonFile, inside } from './config.js';
import { runCommand } from './process.js';
import { artifactFiles, verifyCandidate } from './artifact.js';
import { hashJson, digest } from '../../contracts/src/canonical.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import type { Host } from '../../contracts/src/generated.js';
import { savePreparation, recoverPreparedBuild, compactPreparation, preparationIdForBuild } from './preparations.js';
import { requireStorageSpace } from './storage-space.js';
import { runtimePackageLocations, workspacePackageLocations } from './runtime-dependencies.js';
import { preparationTools } from './preparation-tools.js';
import { mapConcurrent } from './concurrency.js';
import { touchDependencyCache, useDependencyCache } from './dependency-cache.js';
import { createPackageArchive } from './package-archive.js';

async function copyDirectory(source:string,destination:string,options:Parameters<typeof cp>[2]={}):Promise<void>{
  await mkdir(destination,{recursive:true});
  await mapConcurrent(await readdir(source),8,async name=>cp(join(source,name),join(destination,name),{...options,recursive:true}));
}
async function copySourceTree(source: string, destination: string): Promise<void> {
  const directories = [''], files: string[] = [];
  for (let index = 0; index < directories.length;) {
    const batch = directories.slice(index, index + 8); index += batch.length;
    const nested = await mapConcurrent(batch, 8, async local => {
      const children: string[] = [];
      for (const entry of await readdir(join(source, local), { withFileTypes: true })) {
        const child = join(local, entry.name);
        requireThat(!entry.isSymbolicLink(), 'artifact_changed', 'Fixed source snapshots cannot contain filesystem links.');
        if (entry.isDirectory()) children.push(child);
        else { requireThat(entry.isFile(), 'artifact_changed', 'Fixed source snapshot contains an unsupported entry.'); files.push(child); }
      }
      return children;
    });
    for (const children of nested) directories.push(...children);
  }
  await mapConcurrent(directories, 8, local => mkdir(join(destination, local), { recursive: true }));
  await mapConcurrent(files, 8, local => copyFile(join(source, local), join(destination, local)));
}
async function linkDependencyDirectory(source: string, destination: string): Promise<void> {
  await mkdir(destination, { recursive: true });
  const linked = async (entry: Dirent, from: string, to: string): Promise<void> => {
    if (entry.isDirectory()) {
      const target = process.platform === 'win32' ? from : relative(dirname(to), from);
      await symlink(target, to, process.platform === 'win32' ? 'junction' : 'dir'); return;
    }
    if (entry.isSymbolicLink()) { await cp(from, to, { recursive: true, dereference: false, verbatimSymlinks: true }); return; }
    requireThat(entry.isFile(), 'artifact_changed', 'Dependency cache contains an unsupported entry.');
    try { await link(from, to); }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EXDEV') throw error;
      await copyFile(from, to);
    }
  };
  await mapConcurrent(await readdir(source, { withFileTypes: true }), 8, async entry => {
    const from = join(source, entry.name), to = join(destination, entry.name);
    if (entry.isDirectory() && entry.name.startsWith('@')) {
      await mkdir(to, { recursive: true });
      await mapConcurrent(await readdir(from, { withFileTypes: true }), 8, async child =>
        linked(child, join(from, child.name), join(to, child.name)));
      return;
    }
    await linked(entry, from, to);
  });
}

type WorkspaceLink = { location: string; target: string };
type DependencyCache = { schemaVersion: 3; key: string; externalOnly: true; workspaceLinks: WorkspaceLink[] };
const dependencyCacheKey = (lockHash: string, packageManager: string) => hashJson({ cacheSchemaVersion: 3, lockHash, os: process.platform, arch: process.arch, node: process.version, packageManager });
const workspaceLinkEntries = (links: ReadonlyMap<string, string>): WorkspaceLink[] => [...links.entries()]
  .map(([location, target]) => ({ location, target })).sort((a, b) => a.location.localeCompare(b.location));
const sameWorkspaceLinks = (left: readonly WorkspaceLink[], right: readonly WorkspaceLink[]) => hashJson(left) === hashJson(right);
function validCache(metadata: unknown, key: string, links: ReadonlyMap<string, string>): metadata is DependencyCache {
  if (!metadata || typeof metadata !== 'object' || Array.isArray(metadata)) return false;
  const value = metadata as Partial<DependencyCache>;
  return value.schemaVersion === 3 && value.key === key && value.externalOnly === true && Array.isArray(value.workspaceLinks) &&
    value.workspaceLinks.every(link => link && typeof link.location === 'string' && typeof link.target === 'string') &&
    sameWorkspaceLinks(value.workspaceLinks, workspaceLinkEntries(links));
}
function isWorkspacePath(local: string, links: ReadonlyMap<string, string>): boolean {
  for (const location of links.keys()) if (local === location || local.startsWith(location + '/')) return true;
  return false;
}
async function restoreWorkspaceLinks(sourceRoot: string, links: ReadonlyMap<string, string>): Promise<void> {
  for (const [location, target] of links) {
    const source = join(sourceRoot, ...target.split('/')), destination = join(sourceRoot, ...location.split('/'));
    requireThat(inside(sourceRoot, source) && inside(sourceRoot, destination), 'target_conflict', 'Workspace dependency link leaves the build workspace.');
    const sourceMetadata = await stat(source);
    requireThat(sourceMetadata.isDirectory() && inside(sourceRoot, await realpath(source)), 'artifact_changed', 'Workspace dependency source leaves its build workspace.');
    await rm(destination, { recursive: true, force: true }); await mkdir(dirname(destination), { recursive: true });
    const linkTarget = process.platform === 'win32' ? source : relative(dirname(destination), source);
    await symlink(linkTarget, destination, process.platform === 'win32' ? 'junction' : 'dir');
  }
}
async function restoreDependencies(sourceRoot: string, cacheRoot: string, key: string, links: ReadonlyMap<string, string>): Promise<boolean> {
  try {
    const metadata = await jsonFile<DependencyCache>(join(cacheRoot, 'cache.json'), 4096);
    requireThat(validCache(metadata, key, links), 'build_mismatch', 'Dependency cache identity differs from the current lock/runtime key.');
    await stat(join(cacheRoot, 'node_modules'));
    // Build workspaces consume the host-owned cache without copying every package byte.
    // The published candidate is still an independent ordinary-file copy below.
    await linkDependencyDirectory(join(cacheRoot, 'node_modules'), join(sourceRoot, 'node_modules'));
    await restoreWorkspaceLinks(sourceRoot, links);
    return true;
  } catch (error) {
    if (error instanceof IvyError && error.code === 'build_mismatch') return false;
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false;
    throw error;
  }
}
async function retainDependencies(sourceRoot: string, cacheRoot: string, key: string, links: ReadonlyMap<string, string>): Promise<void> {
  const modules = join(sourceRoot, 'node_modules');
  try { await stat(modules); } catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return; throw error; }
  try {
    const existing = await jsonFile<DependencyCache>(join(cacheRoot, 'cache.json'), 4096);
    if (validCache(existing, key, links)) return;
  } catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT' && !(error instanceof IvyError)) throw error; }
  await mkdir(dirname(cacheRoot), { recursive: true });
  const temporary = cacheRoot + '-' + randomUUID() + '.tmp';
  await mkdir(temporary, { recursive: true });
  try {
    // Cache only external packages. Workspace packages remain linked to the freshly
    // copied source tree, so changing @ivy/* content cannot be hidden by this cache.
    try {
      await rename(modules, join(temporary, 'node_modules'));
      for (const location of links.keys()) await rm(join(temporary, ...location.split('/')), { recursive: true, force: true });
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'EXDEV') throw error;
      await copyDirectory(modules, join(temporary, 'node_modules'), { dereference: false, verbatimSymlinks: true, filter: async path => {
        const local = relative(sourceRoot, path).replaceAll('\\', '/'); return !isWorkspacePath(local, links);
      } });
    }
    await atomicJson(join(temporary, 'cache.json'), { schemaVersion: 3, key, externalOnly: true, workspaceLinks: workspaceLinkEntries(links) } satisfies DependencyCache);
    try { await rename(temporary, cacheRoot); }
    catch (error) {
      if (!['EEXIST', 'ENOTEMPTY'].includes((error as NodeJS.ErrnoException).code ?? '')) throw error;
      const current = await jsonFile<DependencyCache>(join(cacheRoot, 'cache.json'), 4096).catch(() => null);
      if (validCache(current, key, links)) { await rm(temporary, { recursive: true, force: true }); return; }
      await rm(cacheRoot, { recursive: true, force: true });
      await rename(temporary, cacheRoot);
    }
    await rm(modules, { recursive: true, force: true });
    await linkDependencyDirectory(join(cacheRoot, 'node_modules'), modules);
    await restoreWorkspaceLinks(sourceRoot, links);
  } catch (error) {
    await rm(temporary, { recursive: true, force: true }).catch(() => undefined);
    throw error;
  }
}

/** Only a different filesystem needs another complete copy of a finished staged payload. */
async function transferPreparedPayload(source: string, destination: string, directory: boolean): Promise<void> {
  try { await rename(source, destination); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EXDEV') throw error;
    if (directory) await cp(source, destination, { recursive: true });
    else await copyFile(source, destination);
  }
}

async function emittedRuntimePackages(root: string): Promise<Set<string>> {
  const packages = new Set<string>(), directories = [root];
  const add = (specifier: string): void => {
    if (specifier.startsWith('.') || specifier.startsWith('/') || specifier.startsWith('node:')) return;
    const parts = specifier.split('/'), name = specifier.startsWith('@') ? parts.slice(0, 2).join('/') : parts[0]!;
    if (/^(?:@[a-z0-9_~-][a-z0-9._~-]*\/)?[a-z0-9_~-][a-z0-9._~-]*$/i.test(name)) packages.add(name);
  };
  for (let index = 0; index < directories.length;) {
    const directory = directories[index++]!;
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) { directories.push(path); continue; }
      if (!entry.isFile() || !/\.(?:c|m)?js$/.test(entry.name)) continue;
      const source = await readFile(path, 'utf8');
      const parsed = typescript.createSourceFile(path, source, typescript.ScriptTarget.Latest, false, typescript.ScriptKind.JS);
      const visit = (node: import('typescript').Node): void => {
        if ((typescript.isImportDeclaration(node) || typescript.isExportDeclaration(node)) && node.moduleSpecifier && typescript.isStringLiteralLike(node.moduleSpecifier)) add(node.moduleSpecifier.text);
        if (typescript.isCallExpression(node) && node.arguments.length && typescript.isStringLiteralLike(node.arguments[0]!) &&
          (node.expression.kind === typescript.SyntaxKind.ImportKeyword || typescript.isIdentifier(node.expression) && node.expression.text === 'require')) add(node.arguments[0]!.text);
        typescript.forEachChild(node, visit);
      };
      visit(parsed);
    }
  }
  return packages;
}
const selectedDependencyPath = (path: string, selected: ReadonlySet<string>): boolean =>
  path === 'node_modules' || [...selected].some(location => path === location || path.startsWith(location + '/') || location.startsWith(path + '/'));

async function copyRuntimeDependencies(sourceRoot: string, artifactRoot: string, selected: ReadonlySet<string>, cacheRoot: string): Promise<void> {
  if (!selected.size) return;
  const selectedRoots = [...selected].filter(location => ![...selected].some(parent =>
    parent !== location && location.startsWith(parent + '/node_modules/'))).sort();
  const roots: string[] = [];
  for (const location of selectedRoots) {
    const installed = join(sourceRoot, ...location.split('/'));
    try { await realpath(installed); roots.push(location); }
    catch (error) {
      // npm lockfiles retain optional/peer packages for other platforms. The exact installed
      // tree is authoritative for those entries; required missing packages fail at launch.
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    }
  }
  await mapConcurrent([...new Set(roots.map(location => dirname(join(artifactRoot, ...location.split('/')))))], 8,
    directory => mkdir(directory, { recursive: true }));
  await mapConcurrent(roots, 8, async location => {
    const from = join(sourceRoot, ...location.split('/')), to = join(artifactRoot, ...location.split('/'));
    requireThat(inside(join(sourceRoot, 'node_modules'), from) && inside(join(artifactRoot, 'node_modules'), to),
      'artifact_changed', 'Runtime dependency leaves its package root.');
    const actualRoot = await realpath(from);
    requireThat(inside(sourceRoot, actualRoot) || inside(cacheRoot, actualRoot),
      'artifact_changed', 'Dependency link leaves its build workspace or host-owned dependency cache.');
    const directories = [{ from: actualRoot, to, local: location }], files: Array<{ from: string; to: string }> = [];
    for (let index = 0; index < directories.length;) {
      const batch = directories.slice(index, index + 8); index += batch.length;
      const nested = await mapConcurrent(batch, 8, async directory => {
        await mkdir(directory.to, { recursive: true });
        const children: typeof directories = [];
        for (const entry of await readdir(directory.from, { withFileTypes: true })) {
          const local = (directory.local + '/' + entry.name).replaceAll('\\', '/');
          if (local.split('/').some(part => ['.bin', '.cache', '.local'].includes(part)) || local.endsWith('.map') ||
            !selectedDependencyPath(local, selected)) continue;
          const raw = join(directory.from, entry.name), destination = join(directory.to, entry.name);
          if (entry.isSymbolicLink()) {
            const actual = await realpath(raw);
            requireThat(inside(sourceRoot, actual) || inside(cacheRoot, actual),
              'artifact_changed', 'Nested dependency link leaves its build workspace or host-owned dependency cache.');
            if ((await stat(actual)).isDirectory()) children.push({ from: actual, to: destination, local });
            else files.push({ from: actual, to: destination });
          } else if (entry.isDirectory()) children.push({ from: raw, to: destination, local });
          else { requireThat(entry.isFile(), 'artifact_changed', 'Runtime dependency contains an unsupported entry.'); files.push({ from: raw, to: destination }); }
        }
        return children;
      });
      for (const children of nested) directories.push(...children);
    }
    await mapConcurrent(files, 8, async file => {
      await copyFile(file.from, file.to);
      if (process.platform !== 'win32') await chmod(file.to, (await stat(file.from)).mode);
    });
  });
}

function lockedPackageName(location: string): string {
  const marker = '/node_modules/', nested = location.lastIndexOf(marker);
  const suffix = nested >= 0 ? location.slice(nested + marker.length) : location.slice('node_modules/'.length);
  const parts = suffix.split('/'); return parts[0]!.startsWith('@') ? parts.slice(0, 2).join('/') : parts[0]!;
}
const intersects = (value: unknown, supported: ReadonlySet<string>): boolean => value === undefined ||
  Array.isArray(value) && value.some(entry => typeof entry === 'string' && supported.has(entry));

/** Add the supported OS/architecture variants omitted by npm on this build host. */
async function materializePortableDependencies(lock: unknown, selected: ReadonlySet<string>, artifactRoot: string,
  temporaryRoot: string, executables: Record<string, string>): Promise<void> {
  if (!selected.size) return;
  const packages = (lock as { packages?: Record<string, Record<string, unknown>> }).packages;
  requireThat(packages && typeof packages === 'object', 'invalid_arguments', 'Portable packaging requires exact npm package metadata.');
  const supportedOs = new Set(['win32', 'linux']), supportedCpu = new Set(['x64', 'arm64']);
  const missing: Array<{ location: string; resolved: string; integrity: string; version: string }> = [];
  for (const location of selected) {
    if (await stat(join(artifactRoot, ...location.split('/'))).then(() => true, error => { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false; throw error; })) continue;
    const metadata = packages[location], constrained = metadata && (metadata['os'] !== undefined || metadata['cpu'] !== undefined);
    if (!constrained || !intersects(metadata['os'], supportedOs) || !intersects(metadata['cpu'], supportedCpu)) continue;
    requireThat(typeof metadata['resolved'] === 'string' && /^https:\/\//.test(metadata['resolved']) &&
      typeof metadata['integrity'] === 'string' && /^sha512-[A-Za-z0-9+/]+={0,2}$/.test(metadata['integrity']) &&
      typeof metadata['version'] === 'string', 'artifact_missing', 'Portable runtime dependency lacks pinned registry bytes.');
    missing.push({ location, resolved: metadata['resolved'], integrity: metadata['integrity'], version: metadata['version'] });
  }
  await mapConcurrent(missing, 4, async value => {
    const response = await fetch(value.resolved, { signal: AbortSignal.timeout(120_000) });
    requireThat(response.ok, 'service_unavailable', 'Pinned portable runtime dependency is unavailable.');
    const declared = Number(response.headers.get('content-length') ?? '0');
    requireThat(!declared || declared <= 64 * 1024 * 1024, 'content_too_large', 'Portable runtime dependency exceeds its package limit.');
    const bytes = Buffer.from(await response.arrayBuffer());
    requireThat(bytes.length > 0 && bytes.length <= 64 * 1024 * 1024 &&
      `sha512-${createHash('sha512').update(bytes).digest('base64')}` === value.integrity,
    'artifact_changed', 'Portable runtime dependency checksum differs from the lockfile.');
    const key = createHash('sha256').update(value.location).digest('hex'), archive = join(temporaryRoot, key + '.tgz');
    const destination = join(artifactRoot, ...value.location.split('/')); await writeFile(archive, bytes, { flag: 'wx' });
    try {
      const listing = await runCommand({ executable: 'tar', args: ['-tzf', archive], timeoutMs: 60_000 }, temporaryRoot, executables, { useJobLauncher: false });
      const entries = listing.stdout.split(/\r?\n/).filter(Boolean);
      requireThat(!listing.truncated && entries.length > 0 && entries.length <= 50_000 && entries.every(raw => {
        const entry = raw.replace(/\/$/, ''); return entry === 'package' || entry.startsWith('package/') &&
          !entry.includes('\\') && entry.split('/').every(part => part !== '..' && part !== '.');
      }), 'artifact_changed', 'Portable runtime dependency archive is unsafe.');
      await mkdir(destination, { recursive: true });
      await runCommand({ executable: 'tar', args: ['-xzf', archive, '-C', destination, '--strip-components', '1'], timeoutMs: 60_000 }, temporaryRoot, executables, { useJobLauncher: false });
      const installed = await jsonFile<{ name: string; version: string }>(join(destination, 'package.json'));
      requireThat(installed.name === lockedPackageName(value.location) && installed.version === value.version,
        'build_mismatch', 'Portable runtime dependency identity differs from the lockfile.');
    } finally { await rm(archive, { force: true }).catch(() => undefined); }
  });
}

export async function prepareCandidate(snapshot: Host.SourceSnapshot, componentId: string, config: Host.HostConfig, bootstrapRoot: string): Promise<Host.Candidate> {
  requireThat(/^[a-z][a-z0-9-]*$/.test(componentId), 'invalid_arguments', 'Component ID is not a local source plan name.');
  // The source snapshot was byte-checked while it was captured and is now a
  // private immutable staging object. Validate its publication metadata here;
  // a second complete source inventory belongs only to explicit recovery checks.
  await verifySnapshotMetadata(snapshot, config);
  const plan = await sourceBuildPlan(snapshot.sourceRoot, componentId);
  if (plan.kind === 'app') requireThat(plan.app?.appId === componentId && plan.app.dist === `dist/apps/${componentId}`,
    'invalid_arguments', 'App preparation requires its exact contained bundle path.');
  requireThat(plan.checks.length === 0, 'invalid_arguments', 'Component plans do not define general verification checks.');
  const packageJson = await jsonFile<{ packageManager: string }>(join(snapshot.sourceRoot, 'package.json'));
  requireThat(/^npm@\d+\.\d+\.\d+$/.test(packageJson.packageManager), 'invalid_arguments', 'The distribution needs a pinned npm package manager.');
  // Caller log wrappers use distinct TEMP directories. Build commands get one explicit host-owned
  // temporary root so identical invocations retain their actual execution identity across calls.
  const temporaryRoot = join(config.stagingRoot, 'command-temp'); await mkdir(temporaryRoot, { recursive: true });
  requireThat(inside(await realpath(config.stagingRoot), await realpath(temporaryRoot)), 'target_conflict', 'Preparation temporary storage leaves staging.');
  const tools = await preparationTools(config.executables, bootstrapRoot, temporaryRoot);
  const dependencyLockHash = digest(await readFile(join(snapshot.sourceRoot, 'package-lock.json')));
  const dependencyKey = dependencyCacheKey(dependencyLockHash, packageJson.packageManager);
    const dependencyCacheRoot = join(config.stagingRoot, 'dependency-cache', dependencyKey.slice('sha256:'.length));
  // Snapshot IDs identify capture operations, not their contents. Derive the logical release from
  // the fixed copied bytes; build recipes validate their pinned package managers and SDKs instead
  // of turning mutable host paths or ambient environment into release identity.
  // File permissions differ between Windows and Linux checkouts and have no source meaning.
  // Package requirements and the portable dependency materializer own runtime compatibility.
  const sourceIdentity = hashJson((await artifactFiles(snapshot.sourceRoot)).map(({ path, hash, bytes }) => ({ path, hash, bytes })));
  const buildId = candidateBuildId(componentId, plan.version, sourceIdentity);
  const progress = new PreparationProgress(config, componentId, snapshot.snapshotId);
  let lock: ExecutorLock | null = null, cacheLease: ExecutorLock | null = null;
  let journal: HostJournal | null = null;
  let record: Host.PreparationRecord | null = null, registered: Host.Candidate | null = null;
  try {
    await progress.step('checking-existing-candidate');
    lock = new ExecutorLock(join(config.stagingRoot, 'build-locks', buildId.slice('sha256:'.length)));
    journal = new HostJournal(config);
    const recovered = await recoverPreparedBuild(journal, buildId);
    if (recovered) { await verifyCandidate(recovered, config); return recovered; }
    try { const previous = journal.candidateForBuild(componentId, buildId); await verifyCandidate(previous, config); return previous; }
    catch (error) { if (!(error instanceof IvyError) || error.code !== 'not_found') throw error; }
    await mkdir(config.artifactRoot, { recursive: true });
    await requireStorageSpace(config.stagingRoot, 512 * 1024 * 1024);
    await requireStorageSpace(config.artifactRoot, 256 * 1024 * 1024);
    const preparationId = preparationIdForBuild(buildId), root = join(config.stagingRoot, 'build-' + preparationId), sourceRoot = join(root, 'source'), artifactRoot = join(root, 'artifact');
    await mkdir(root, { recursive: true });
    // A failed attempt keeps its bounded preparation record for diagnosis. Reusing its mutable
    // source/artifact trees would make the same operation fail on stale links or partial output.
    await rm(sourceRoot, { recursive: true, force: true });
    await rm(artifactRoot, { recursive: true, force: true });
    record = { schemaVersion: 1, preparationId, componentId, buildId, snapshot, phase: 'building', candidate: null, startedAt: new Date().toISOString(), updatedAt: new Date().toISOString(), errorCode: null };
    await savePreparation(config, record);
    const executionEnvironment: Record<string, string> = { ...tools.environment, IVY_BUILD_ID: buildId,
      IVY_BUILD_CACHE_ROOT: join(config.stagingRoot, 'build-cache') };
    await progress.step('copying-source');
    await copySourceTree(snapshot.sourceRoot, sourceRoot);
    const packageLock = await jsonFile<unknown>(join(sourceRoot, 'package-lock.json'));
    const workspaceLinks = workspacePackageLocations(packageLock);
    cacheLease = await useDependencyCache(config.stagingRoot, dependencyKey.slice(7));
    await touchDependencyCache(dependencyCacheRoot);
    const dependencyCacheHit = await restoreDependencies(sourceRoot, dependencyCacheRoot, dependencyKey, workspaceLinks);
    executionEnvironment['IVY_DEPENDENCY_CACHE_HIT'] = dependencyCacheHit ? '1' : '0';
    const execution = { onOutput: progress.output, jobLauncher: join(bootstrapRoot, 'dist/native/ivy-job.exe'), environment: executionEnvironment };
    for (const [index, command] of plan.prepare.entries()) {
      await progress.step(`prepare-${index}`, { command: [command.executable, ...command.args].join(' ') });
      await runCommand(command, sourceRoot, config.executables, execution);
      if (!dependencyCacheHit && command.executable === 'node' && command.args[0] === 'tools/build/run-npm.mjs' && command.args[1] === 'ci')
        await retainDependencies(sourceRoot, dependencyCacheRoot, dependencyKey, workspaceLinks);
    }
    const runtimePackages = plan.kind === 'app' ? new Set<string>() : runtimePackageLocations(packageLock, await emittedRuntimePackages(join(sourceRoot, 'dist')));
    await mkdir(artifactRoot, { recursive: true });
    await progress.step('collecting-runtime-files');
    if (plan.kind === 'app') {
      const app = plan.app!, sourceBundle = resolve(sourceRoot, app.dist), destinationBundle = resolve(artifactRoot, app.dist);
      const metadata = await stat(sourceBundle);
      requireThat(metadata.isDirectory() && inside(join(sourceRoot, 'dist'), await realpath(sourceBundle)) && inside(artifactRoot, destinationBundle),
        'artifact_missing', 'Build did not produce the selected ui bundle.');
      await copyDirectory(sourceBundle, destinationBundle, { recursive: true, dereference: true, filter: async path => {
        const actual = await realpath(path);
        requireThat(inside(sourceBundle, actual), 'artifact_changed', 'UI bundle link leaves its selected output.');
        return !path.endsWith('.map');
      } });
    } else {
      for (const name of ['dist', 'node_modules', 'package.json', 'package-lock.json']) {
        const from = join(sourceRoot, name), exists = await stat(from).then(() => true, error => { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return false; throw error; });
        if (!exists) { requireThat(name === 'node_modules', 'artifact_missing', 'Build did not produce its executable output.'); continue; }
        const options = { recursive: true, dereference: true, filter: async (path:string) => {
          const local = relative(sourceRoot, path).replaceAll('\\', '/');
          if (local.split('/').some(part => ['.bin', '.cache', '.local'].includes(part))) return false;
          if (local.endsWith('.map') || local.startsWith('tests/')) return false;
          if (name === 'node_modules' && !selectedDependencyPath(local, runtimePackages)) return false;
          const actual = await realpath(path);
          requireThat(inside(sourceRoot, actual) || name === 'node_modules' && inside(dependencyCacheRoot, actual),
            'artifact_changed', 'Dependency/artifact link leaves its build workspace or host-owned dependency cache.'); return true;
        } };
        if (name === 'node_modules') await copyRuntimeDependencies(sourceRoot, artifactRoot, runtimePackages, dependencyCacheRoot);
        else if((await stat(from)).isDirectory())await copyDirectory(from,join(artifactRoot,name),options);
        else await cp(from,join(artifactRoot,name),options);
      }
      await materializePortableDependencies(packageLock, runtimePackages, artifactRoot, temporaryRoot, config.executables);
    }
    await atomicJson(join(artifactRoot, 'dist/build-info.json'), { schemaVersion: 1, componentId, version: plan.version, buildId });
    if (plan.kind !== 'app') requireThat(plan.entrypoint && plan.readiness && plan.shutdown && plan.restart, 'artifact_missing', 'Executable release plan is incomplete.');
    const manifest: Host.ReleaseManifest = { schemaVersion: 1, componentId, kind: plan.kind, version: plan.version, buildId,
      connectsToHive: plan.connectsToHive, requirements: plan.requirements,
      ...(plan.kind === 'app' ? { app: plan.app! } : { entrypoint: plan.entrypoint!, readiness: plan.readiness!, shutdown: plan.shutdown!, restart: plan.restart! }),
      ...(plan.storage ? { storage: plan.storage } : {}), ...(plan.runtimeReset ? { runtimeReset: plan.runtimeReset } : {}) };
    validateHost('ReleaseManifest', manifest);
    const candidateId = buildId;
    const destination = join(config.artifactRoot, 'candidates', candidateId.slice('sha256:'.length));
    const manifestPath = join(destination, 'component.json');
    let candidate: Host.Candidate = { candidateId, componentId, buildId,
      artifactRoot: join(destination, 'artifact'), manifestPath, createdAt: new Date().toISOString(),
      platform: { os: process.platform as 'win32' | 'linux', arch: process.arch as 'x64' | 'arm64', node: process.version } };
    await progress.step('publishing');
    record = { ...record, phase: 'publishing', candidate, updatedAt: new Date().toISOString() }; await savePreparation(config, record);
    const incoming = join(config.artifactRoot, 'incoming-' + randomUUID()); await mkdir(incoming, { recursive: true });
    requireThat(inside(await realpath(config.stagingRoot), await realpath(root)) && inside(await realpath(root), await realpath(artifactRoot)) &&
      inside(await realpath(config.artifactRoot), await realpath(incoming)) &&
      inside(config.artifactRoot, destination), 'target_conflict', 'Candidate promotion leaves its configured roots.');
    await transferPreparedPayload(artifactRoot, join(incoming, 'artifact'), true);
    await atomicJson(join(incoming, 'component.json'), manifest);
    // The final same-volume rename publishes a complete candidate even when staging is on another disk.
    await mkdir(join(config.artifactRoot, 'candidates'), { recursive: true });
    await rename(incoming, destination);
    const archive = await createPackageArchive(candidate, config.executables);
    candidate = { ...candidate, archivePath: archive.path, archiveHash: archive.hash };
    record = { ...record, candidate, updatedAt: new Date().toISOString() }; await savePreparation(config, record);
    await progress.step('verifying-published-artifact');
    journal.saveCandidate(candidate, manifest); registered = candidate;
    record = { ...record, phase: 'verified', updatedAt: new Date().toISOString() }; await savePreparation(config, record);
    await progress.step('cleaning-staging');
    await compactPreparation(journal, record, true); await progress.step('complete'); return candidate;
  } catch (error) {
    const failure = IvyError.from(error);
    await progress.step('failed: ' + failure.code);
    if (record) await savePreparation(config, { ...record, phase: registered ? 'verified' : failure.outcome === 'unknown' || record.phase === 'publishing' ? 'unknown' : 'failed', updatedAt: new Date().toISOString(), errorCode: failure.code }).catch(() => undefined);
    if (registered) return registered;
    throw new IvyError(failure.code, failure.message, failure.outcome, { snapshotId: snapshot.snapshotId });
  } finally { await progress.close(); cacheLease?.close(); journal?.close(); lock?.close(); }
}
