import { createHash } from 'node:crypto';
import { lstat, realpath, open, readdir } from 'node:fs/promises';
import { join, relative } from 'node:path';
import { jsonFile, inside } from './config.js';
import { validateHost } from '../../contracts/src/host-validation.js';
import { requireThat } from '../../contracts/src/errors.js';
import type { Host } from '../../contracts/src/generated.js';
import { mapConcurrent } from './concurrency.js';
import { requireRuntimeRequirements } from './package-requirements.js';
import { portableNativeFiles } from './portable-native-files.js';

async function hashFile(path: string, buffer: Buffer): Promise<string> {
  const hash = createHash('sha256'), file = await open(path, 'r');
  try {
    for (;;) {
      const { bytesRead } = await file.read(buffer, 0, buffer.length, null);
      if (bytesRead === 0) break;
      hash.update(buffer.subarray(0, bytesRead));
    }
    return 'sha256:' + hash.digest('hex');
  } finally { await file.close(); }
}
// Large release tools (notably node.exe on Windows) are otherwise split into
// thousands of asynchronous 64 KiB reads.  The per-read filesystem overhead
// dominated candidate preparation even though hashing itself is cheap.
export const fileHash = (path: string): Promise<string> => hashFile(path, Buffer.allocUnsafe(4 * 1024 * 1024));

/** The only generic archive checksum boundary: bytes arriving from another host. */
export async function verifyTransferredArchive(path: string, expectedHash: string): Promise<void> {
  requireThat(/^sha256:[0-9a-f]{64}$/.test(expectedHash), 'invalid_arguments', 'Transferred archive needs an explicit SHA-256 checksum.');
  const metadata = await lstat(path);
  requireThat(metadata.isFile() && !metadata.isSymbolicLink(), 'artifact_changed', 'Transferred archive must be a regular file.');
  requireThat(await fileHash(path) === expectedHash, 'artifact_changed', 'Transferred candidate archive changed.');
}

/** Read the runtime contract shared by retained and newly written manifests.
 * Historical preparation/provenance fields have no launch meaning. */
export function releaseManifest(input: unknown): Host.ReleaseManifest {
  const value = input as Host.ReleaseManifest;
  const manifest: Host.ReleaseManifest = { schemaVersion: value.schemaVersion, componentId: value.componentId, kind: value.kind, version: value.version,
    buildId: value.buildId, connectsToHive: value.connectsToHive, requirements: value.requirements,
    ...(value.app === undefined ? {} : { app: value.app }), ...(value.entrypoint === undefined ? {} : { entrypoint: value.entrypoint }),
    ...(value.readiness === undefined ? {} : { readiness: value.readiness }), ...(value.shutdown === undefined ? {} : { shutdown: value.shutdown }),
    ...(value.restart === undefined ? {} : { restart: value.restart }), ...(value.storage === undefined ? {} : { storage: value.storage }),
    ...(value.runtimeReset === undefined ? {} : { runtimeReset: value.runtimeReset }) };
  validateHost('ReleaseManifest', manifest); return manifest;
}
export async function readReleaseManifest(path: string): Promise<Host.ReleaseManifest> {
  return releaseManifest(await jsonFile<unknown>(path));
}

/** Deterministic contained-file inventory used for package and source identity.
 * It is not part of normal candidate activation. */
export async function artifactFiles(root: string): Promise<Array<{ path: string; hash: string; bytes: number; mode: number }>> {
  const files: { path: string; local: string }[] = [], directories = [root];
  for (let index = 0; index < directories.length;) {
    const batch = directories.slice(index, index + 4); index += batch.length;
    const children = await mapConcurrent(batch, 4, async directory => {
      const nested: string[] = [];
      for (const entry of await readdir(directory, { withFileTypes: true })) {
        const path = join(directory, entry.name), local = relative(root, path).replaceAll('\\', '/');
        requireThat(inside(root, path) && !entry.isSymbolicLink(), 'artifact_changed', 'Package comparison accepts contained files and directories only.');
        if (entry.isDirectory()) nested.push(path);
        else { requireThat(entry.isFile(), 'artifact_changed', 'Package comparison found an unsupported entry.'); files.push({ path, local }); }
      }
      return nested;
    });
    for (const nested of children) directories.push(...nested);
  }
  requireThat(files.length <= 50_000, 'limit_exceeded', 'Package comparison exceeds its file limit.');
  const result = await mapConcurrent(files, 4, async ({ path, local }) => {
    const metadata = await lstat(path);
    return { path: local, hash: await fileHash(path), bytes: metadata.size, mode: metadata.mode & 0o777 };
  });
  return result.sort((a, b) => a.path.localeCompare(b.path));
}
export async function verifyCandidate(candidate: Host.Candidate, config: Pick<Host.HostConfig, 'artifactRoot'>): Promise<Host.ReleaseManifest> {
  validateHost('Candidate', candidate);
  for (const path of [candidate.artifactRoot, candidate.manifestPath]) requireThat(inside(config.artifactRoot, await realpath(path)), 'target_conflict', 'Candidate leaves its explicit host artifact root.');
  const manifest = await readReleaseManifest(candidate.manifestPath);
  if (manifest.kind !== 'app') requireRuntimeRequirements(manifest.requirements);
  requireThat(manifest.buildId === candidate.buildId && manifest.componentId === candidate.componentId,
    'build_mismatch', 'Candidate manifest does not identify its release.');
  await verifyLaunchCandidate(candidate, config, manifest.entrypoint?.args[0]);
  if (process.platform === 'win32') for (const file of portableNativeFiles(manifest.componentId)) {
    const path = join(candidate.artifactRoot, ...file.split('/')), metadata = await lstat(path).catch(() => null);
    requireThat(metadata?.isFile() && !metadata.isSymbolicLink() && metadata.size > 0 && inside(candidate.artifactRoot, await realpath(path)),
      'artifact_missing', 'Windows candidate is missing its required native runtime payload.');
  }
  return manifest;
}

/** A private immutable release needs only its identity stamp and selected entry. */
export async function verifyLaunchCandidate(candidate: Host.Candidate, config: Pick<Host.HostConfig,'artifactRoot'>, entrypoint: string | undefined): Promise<void> {
  validateHost('Candidate',candidate);
  requireThat(inside(config.artifactRoot,await realpath(candidate.artifactRoot)),'target_conflict','Launch leaves its artifact root.');
  for(const selected of ['dist/build-info.json',...(entrypoint?.startsWith('dist/')?[entrypoint]:[])]){
    const local=join(candidate.artifactRoot,selected),metadata=await lstat(local);
    requireThat(metadata.isFile()&&!metadata.isSymbolicLink()&&inside(candidate.artifactRoot,await realpath(local)),'artifact_changed','Launch entry or stamp is unavailable.');
  }
  const info=await jsonFile<{buildId:string}>(join(candidate.artifactRoot,'dist/build-info.json'));
  requireThat(info.buildId===candidate.buildId,'build_mismatch','Launch build differs from its prepared identity.');
}
