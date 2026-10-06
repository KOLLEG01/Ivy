import assert from 'node:assert/strict';
import { readFile, mkdir, lstat, realpath, copyFile, rename } from 'node:fs/promises';
import { constants } from 'node:fs';
import { join, resolve, dirname } from 'node:path';
import { parseArgs } from 'node:util';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';
import { hostConfig } from '../../../dist/packages/host-runtime/src/host-config.js';
import { atomicJson, inside } from '../../../dist/packages/host-runtime/src/config.js';
import { verifyCandidate, verifyTransferredArchive } from '../../../dist/packages/host-runtime/src/artifact.js';
import { runCommand } from '../../../dist/packages/host-runtime/src/process.js';
import { HostJournal } from '../../../dist/packages/host-runtime/src/journal.js';
import { validateHost } from '../../../dist/packages/contracts/src/host-validation.js';
import { hashJson } from '../../../dist/packages/contracts/src/canonical.js';

export async function verifyImageRelocation(candidate, localImage, readBlob) {
  const read = async id => {
    assert.match(id ?? '', /^sha256:[0-9a-f]{64}$/);
    const bytes = await readBlob(id); assert.ok(bytes.length <= 1024 * 1024);
    assert.equal('sha256:' + createHash('sha256').update(bytes).digest('hex'), id);
    return JSON.parse(bytes.toString('utf8'));
  };
  let manifest = await read(candidate.dockerImage);
  if (manifest.mediaType === 'application/vnd.oci.image.index.v1+json') {
    assert.ok(Array.isArray(manifest.manifests) && manifest.manifests.length <= 32);
    const selected = manifest.manifests.filter(m => m.platform?.os === candidate.platform.os && m.platform?.architecture === (candidate.platform.arch === 'x64' ? 'amd64' : candidate.platform.arch));
    assert.equal(selected.length, 1); manifest = await read(selected[0].digest);
  }
  assert.equal(manifest.mediaType, 'application/vnd.oci.image.manifest.v1+json');
  assert.equal(manifest.config?.digest, localImage);
  const config = await read(localImage);
  assert.equal(config.os, candidate.platform.os);
  assert.equal(config.architecture, candidate.platform.arch === 'x64' ? 'amd64' : candidate.platform.arch);
  assert.equal(config.config?.Labels?.['dev.ivy.build'], candidate.buildId);
}

export function verifyTransferredImage(images, candidate, expectedImage) {
  assert.equal(candidate.componentId, 'hive'); assert.equal(candidate.platform.os, 'linux');
  assert.match(expectedImage ?? '', /^sha256:[0-9a-f]{64}$/);
  assert.equal(candidate.dockerImage, expectedImage);
  assert.equal(images.length, 1); assert.equal(images[0].Id, expectedImage);
  assert.equal(images[0].Config.Labels['dev.ivy.build'], candidate.buildId);
  assert.equal(images[0].Os, candidate.platform.os);
  assert.equal(images[0].Architecture, candidate.platform.arch === 'x64' ? 'amd64' : candidate.platform.arch);
}

// Explicit operator transfer from a trusted preparation host. This imports immutable, already
// checked bytes only; deployment remains a separate ordinary CLI operation. The recorded source
// The source path remains the original prepared snapshot; the transferred artifact is immutable.
async function main() {
  const { values } = parseArgs({ options: Object.fromEntries(['config', 'candidate', 'incoming', 'expected-id', 'expected-archive', 'expected-image', 'expected-local-image', 'report'].map(key => [key, { type: 'string' }])) });
  for (const key of ['config', 'candidate', 'incoming', 'expected-id', 'expected-archive', 'report']) assert.ok(values[key], 'Missing --' + key);
  const configPath = resolve(values.config), config = await hostConfig(configPath), configHash = hashJson(config);
  const incoming = await realpath(resolve(values.incoming)), original = JSON.parse(await readFile(resolve(values.candidate), 'utf8'));
  validateHost('Candidate', original); assert.equal(original.candidateId, values['expected-id']); assert.equal(original.candidateId, original.buildId);
  assert.equal(original.platform.os, process.platform); assert.equal(original.platform.arch, process.arch); assert.equal(original.platform.node, process.version);
  const regular = async name => { const path = join(incoming, name), stat = await lstat(path); assert.ok(stat.isFile() && !stat.isSymbolicLink() && stat.nlink === 1); assert.ok(inside(incoming, await realpath(path))); return path; };
  let imported = original;
  if (original.dockerImage !== undefined) {
    assert.equal(original.componentId, 'hive'); assert.equal(process.platform, 'linux');
    assert.match(values['expected-image'] ?? '', /^sha256:[0-9a-f]{64}$/);
    assert.equal(original.dockerImage, values['expected-image'], 'Confirm the exact image transferred separately with docker save/load.');
    const localImage = values['expected-local-image'] ?? original.dockerImage;
    assert.match(localImage, /^sha256:[0-9a-f]{64}$/);
    if (localImage !== original.dockerImage) {
      const imageArchive = await regular('image.tar');
      await verifyImageRelocation(original, localImage, async id => {
        const result = await runCommand({ executable: 'tar', args: ['-xOf', imageArchive, 'blobs/sha256/' + id.slice(7)], timeoutMs: 30000 }, incoming, config.executables);
        assert.equal(result.truncated, false); return Buffer.from(result.stdout, 'utf8');
      });
      imported = { ...original, dockerImage: localImage };
    }
    const result = await runCommand({ executable: 'docker', args: ['image', 'inspect', localImage], timeoutMs: 30000 }, incoming, config.executables);
    verifyTransferredImage(JSON.parse(result.stdout), imported, localImage);
  } else {
    assert.equal(values['expected-image'], undefined, 'Process packages cannot supply an image.');
    assert.equal(values['expected-local-image'], undefined);
  }
  const archive = await regular('artifact.tar.gz'), manifestPath = await regular('component.json');
  await verifyTransferredArchive(archive, values['expected-archive']);
  const manifest = JSON.parse(await readFile(manifestPath, 'utf8')); validateHost('ReleaseManifest', manifest);
  assert.equal(manifest.componentId, original.componentId); assert.equal(manifest.buildId, original.buildId);
  const artifactBase = await realpath(config.artifactRoot), root = join(artifactBase, 'candidates', original.candidateId.slice(7));
  await mkdir(dirname(root), { recursive: true }); assert.ok(inside(artifactBase, await realpath(dirname(root))));
  const candidateAt = base => {
    const { archivePath: _archivePath, archiveHash: _archiveHash, ...retained } = imported;
    return { ...retained, artifactRoot: join(base, 'artifact'), manifestPath: join(base, 'component.json') };
  };
  const candidate = candidateAt(root); let present = false;
  try { const stat = await lstat(root); assert.ok(stat.isDirectory() && !stat.isSymbolicLink()); present = true; } catch (error) { if (error.code !== 'ENOENT') throw error; }
  if (present) {
    await verifyCandidate(candidate, config); assert.equal(hashJson(JSON.parse(await readFile(candidate.manifestPath, 'utf8'))), hashJson(manifest));
  } else {
    const stage = join(artifactBase, 'incoming-transfer-' + original.candidateId.slice(7));
    // A partial prior extraction needs explicit inspection; never overwrite or guess its outcome.
    await mkdir(stage); assert.ok(inside(artifactBase, await realpath(stage))); await mkdir(join(stage, 'artifact'));
    await copyFile(manifestPath, join(stage, 'component.json'), constants.COPYFILE_EXCL);
    const distribution = fileURLToPath(new URL('../../../', import.meta.url));
    await runCommand({ executable: 'tar', args: ['-xzf', archive, '-C', '.'], timeoutMs: 180000 }, stage, config.executables,
      { jobLauncher: join(distribution, 'dist/native/ivy-job.exe') });
    await verifyCandidate(candidateAt(stage), config);
    await rename(stage, root);
  }
  assert.equal(hashJson(await hostConfig(configPath)), configHash, 'The selected host configuration changed during transfer.');
  const journal = new HostJournal(config);
  try { journal.saveCandidate(candidate, manifest); } finally { journal.close(); }
  const report = { schemaVersion: 1, observedAt: new Date().toISOString(), hostId: config.hostId, original, candidate,
    manifestHash: hashJson(manifest), checksumVerified: true, reusedExisting: present, activated: false };
  await atomicJson(resolve(values.report), report); console.log(JSON.stringify({ candidateId: candidate.candidateId, buildId: candidate.buildId, artifactRoot: candidate.artifactRoot, activated: false }));
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main().catch(error => { console.error(JSON.stringify({ code: error.code ?? 'candidate_transfer_failed', message: error.message })); process.exitCode = 1; });
