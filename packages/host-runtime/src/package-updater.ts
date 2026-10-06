import { createWriteStream } from 'node:fs';
import { copyFile, cp, lstat, mkdir, rename, rm } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { Readable } from 'node:stream';
import { pipeline } from 'node:stream/promises';
import { canonical, compareVersions, digest } from '../../contracts/src/canonical.js';
import { IvyError, requireThat } from '../../contracts/src/errors.js';
import type { Host } from '../../contracts/src/generated.js';
import { verifyCandidate } from './artifact.js';
import { inside } from './config.js';
import { ExecutorLock, HostJournal, requiresBootstrapReplacement } from './journal.js';
import { validatePackageArchive, validatePackageCatalog, validatePackageCatalogIndex } from './package-archive.js';
import type { PackageCatalogEntry } from './package-archive.js';
import { runtimeRequirementsSatisfied } from './package-requirements.js';

export interface BootstrapPackageComponent {
  instanceId: string;
  candidate: Host.Candidate;
  manifest: Host.ReleaseManifest;
}
export interface PackageSyncResult { bootstrap: BootstrapPackageComponent[] }

function endpoint(base: string, path: string): string {
  const url = new URL(base), relative = new URL(path, 'http://ivy.invalid'), prefix = url.pathname.replace(/\/$/, '');
  url.pathname = prefix + relative.pathname; url.search = relative.search; url.hash = ''; return url.href;
}

async function moveDirectory(source: string, destination: string): Promise<void> {
  try { await rename(source, destination); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EXDEV') throw error;
    await cp(source, destination, { recursive: true }); await rm(source, { recursive: true, force: true });
  }
}
async function moveFile(source: string, destination: string): Promise<void> {
  try { await rename(source, destination); }
  catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EXDEV') throw error;
    await copyFile(source, destination); await rm(source, { force: true });
  }
}

export class PackageUpdater {
  private revision = 0;
  private readonly pendingBootstrap = new Map<string, BootstrapPackageComponent>();
  constructor(_config: Host.HostConfig, readonly journal: HostJournal, readonly credential: string) {}
  private get config(): Host.HostConfig { return this.journal.config; }

  private async importPackage(entry: PackageCatalogEntry): Promise<Host.Candidate> {
    const lock = new ExecutorLock(join(this.config.stagingRoot, 'build-locks', entry.buildId.slice(7)));
    try { return await this.importLockedPackage(entry); }
    finally { lock.close(); }
  }

  private async importLockedPackage(entry: PackageCatalogEntry): Promise<Host.Candidate> {
    const destination = join(this.config.artifactRoot, 'candidates', entry.buildId.slice('sha256:'.length));
    const candidateAt = (root: string): Host.Candidate => ({ candidateId: entry.buildId, componentId: entry.componentId, buildId: entry.buildId,
      artifactRoot: join(root, 'artifact'), manifestPath: join(root, 'component.json'), archivePath: join(root, 'package.tar.gz'), archiveHash: entry.archiveHash,
      createdAt: entry.publishedAt, platform: { os: process.platform as 'win32' | 'linux', arch: process.arch as 'x64' | 'arm64', node: process.version } });
    try {
      const registered = this.journal.candidate(entry.buildId);
      requireThat(registered.archiveHash === entry.archiveHash, 'artifact_changed', 'Registered package archive differs from Hive.');
      await verifyCandidate(registered, this.config); return registered;
    } catch (error) { if (IvyError.from(error).code !== 'not_found') throw error; }
    try {
      const metadata = await lstat(destination);
      requireThat(metadata.isDirectory() && !metadata.isSymbolicLink(), 'artifact_changed', 'Installed package candidate is invalid.');
      const candidate = candidateAt(destination); await verifyCandidate(candidate, this.config); this.journal.saveCandidate(candidate, entry.manifest); return candidate;
    } catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }
    const temporary = join(this.config.stagingRoot, 'package-' + entry.archiveHash.slice(7));
    const archive = temporary + '.tar.gz', extraction = temporary + '-extract', incoming = join(this.config.artifactRoot, 'incoming-package-' + entry.archiveHash.slice(7));
    await rm(temporary, { recursive: true, force: true }); await rm(extraction, { recursive: true, force: true }); await rm(incoming, { recursive: true, force: true });
    await mkdir(dirname(archive), { recursive: true });
    try {
      const response = await fetch(endpoint(this.config.publicBaseUrl, `/api/v1/packages/${encodeURIComponent(entry.componentId)}/${encodeURIComponent(entry.version)}/artifact`),
        { headers: { authorization: 'Bearer ' + this.credential }, signal: AbortSignal.timeout(5 * 60_000) });
      requireThat(response.ok && response.body, response.status === 404 ? 'not_found' : 'service_unavailable', 'Package artifact download failed.');
      await pipeline(Readable.fromWeb(response.body as import('node:stream/web').ReadableStream), createWriteStream(archive, { flags: 'wx' }));
      await validatePackageArchive(archive, entry.archiveHash, entry.manifest, extraction, this.config.executables);
      await mkdir(dirname(incoming), { recursive: true }); await moveDirectory(extraction, incoming);
      await moveFile(archive, join(incoming, 'package.tar.gz'));
      requireThat(inside(this.config.artifactRoot, destination) && inside(this.config.artifactRoot, incoming), 'target_conflict', 'Package import leaves the host artifact root.');
      await mkdir(dirname(destination), { recursive: true }); await rename(incoming, destination);
      const candidate = candidateAt(destination); await verifyCandidate(candidate, this.config); this.journal.saveCandidate(candidate, entry.manifest); return candidate;
    } finally {
      await rm(archive, { force: true }).catch(() => undefined); await rm(extraction, { recursive: true, force: true }).catch(() => undefined);
      await rm(incoming, { recursive: true, force: true }).catch(() => undefined);
    }
  }

  private result(): PackageSyncResult {
    for (const [instanceId, component] of this.pendingBootstrap) {
      if (this.journal.installed(instanceId)?.candidateId === component.candidate.candidateId) this.pendingBootstrap.delete(instanceId);
    }
    return { bootstrap: [...this.pendingBootstrap.values()] };
  }

  private installedVersion(instanceId: string): string | null {
    const installed = this.journal.installed(instanceId);
    if (!installed) return null;
    try { return this.journal.manifest(installed.candidateId).version; }
    catch (error) {
      // A package is also the upgrade boundary from pre-package releases.
      // Their retained manifests may no longer satisfy the current schema.
      if (IvyError.from(error).code !== 'invalid_arguments') throw error;
      return null;
    }
  }

  async sync(): Promise<PackageSyncResult> {
    const response = await fetch(endpoint(this.config.publicBaseUrl, `/api/v1/packages/catalog?after=${this.revision}`),
      { headers: { authorization: 'Bearer ' + this.credential }, signal: AbortSignal.timeout(5000) });
    if (response.status === 204) return this.result();
    requireThat(response.ok, 'service_unavailable', 'Hive package catalog is unavailable.');
    const index = validatePackageCatalogIndex(await response.json());
    const executor = this.config.instances.find(instance => instance.componentId === 'host-executor');
    if (executor && this.journal.installed(executor.instanceId) && requiresBootstrapReplacement(executor)) {
      let newestExecutor: (typeof index.packages)[number] | null = null;
      for (const entry of index.packages.filter(entry => entry.componentId === 'host-executor')) {
        if (!newestExecutor || compareVersions(entry.version, newestExecutor.version) > 0) newestExecutor = entry;
      }
      if (newestExecutor) {
        // Validate the selected release completely, including its archive identity.
        // Other components may have manifest fields that only this newer executor knows.
        const entry = validatePackageCatalog({ schemaVersion: 1, revision: index.revision, packages: [newestExecutor] }).packages[0]!;
        const current = this.installedVersion(executor.instanceId);
        if ((!current || compareVersions(entry.version, current) > 0) && runtimeRequirementsSatisfied(entry.manifest.requirements)) {
          const candidate = await this.importPackage(entry);
          this.pendingBootstrap.set(executor.instanceId, { instanceId: executor.instanceId, candidate, manifest: entry.manifest });
          this.revision = Math.max(this.revision, index.revision);
          return this.result();
        }
      }
    }
    const catalog = validatePackageCatalog(index);
    const newest = new Map<string, PackageCatalogEntry>();
    for (const entry of catalog.packages) {
      const previous = newest.get(entry.componentId);
      if (!previous || compareVersions(entry.version, previous.version) > 0) newest.set(entry.componentId, entry);
    }
    for (const entry of newest.values()) {
      if (entry.manifest.kind === 'app') continue;
      if (!runtimeRequirementsSatisfied(entry.manifest.requirements)) continue;
      const instances = this.config.instances.filter(instance => instance.componentId === entry.componentId);
      if (!instances.length) continue;
      let candidate: Host.Candidate | null = null;
      for (const instance of instances) {
        const installed = this.journal.installed(instance.instanceId);
        if (installed) {
          const current = this.installedVersion(instance.instanceId);
          if (current && compareVersions(entry.version, current) <= 0) continue;
        }
        // Hive retains its external host updater. Local bootstrap owners use the
        // same imported candidate, then hand off to stopped-owner maintenance.
        if (entry.manifest.kind === 'hive') continue;
        if (installed && requiresBootstrapReplacement(instance)) {
          candidate ??= await this.importPackage(entry);
          this.pendingBootstrap.set(instance.instanceId, { instanceId: instance.instanceId, candidate, manifest: entry.manifest });
          continue;
        }
        candidate ??= await this.importPackage(entry);
        const executorBuildId = this.journal.installed('host-executor')?.buildId ?? null;
        const operationId = 'package-' + digest(canonical({ instanceId: instance.instanceId, version: entry.version, buildId: entry.buildId,
          configurationHash: digest(canonical(this.config)), executorBuildId })).slice(7, 47);
        this.journal.accept({ action: 'deploy', instanceId: instance.instanceId, operationId, candidateId: candidate.candidateId });
      }
    }
    this.revision = Math.max(this.revision, catalog.revision);
    return this.result();
  }
}
