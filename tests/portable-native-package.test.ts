import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { digest } from '../packages/contracts/src/canonical.js';
import type { Host } from '../packages/contracts/src/generated.js';
import { verifyCandidate } from '../packages/host-runtime/src/artifact.js';
import { createPackageArchive, validatePackageArchive } from '../packages/host-runtime/src/package-archive.js';

test('a portable AgentManager package requires its Windows launcher on every publishing host', async () => {
  const root = await mkdtemp(join(tmpdir(), 'ivy-portable-package-'));
  try {
    const candidateRoot = join(root, 'candidate'), artifactRoot = join(candidateRoot, 'artifact');
    const buildId = digest('portable-agent-test');
    const manifest: Host.ReleaseManifest = {
      schemaVersion: 1, componentId: 'agent-manager', kind: 'service', version: '99.0.0', buildId,
      connectsToHive: true, requirements: { node: '>=24.18.0 <25.0.0', hiveProtocol: 1, contracts: [] },
      entrypoint: { executable: 'node', args: ['dist/main.js'], timeoutMs: 30000 },
      readiness: { timeoutMs: 30000, command: { executable: 'node', args: ['dist/health.js'], timeoutMs: 5000 } },
      shutdown: { timeoutMs: 15000 }, restart: { policy: 'always', minimumDelayMs: 1000, maximumDelayMs: 60000 },
    };
    await mkdir(join(artifactRoot, 'dist'), { recursive: true });
    await writeFile(join(artifactRoot, 'dist/build-info.json'), JSON.stringify({ schemaVersion: 1, componentId: 'agent-manager', version: '99.0.0', buildId }));
    await writeFile(join(artifactRoot, 'dist/main.js'), 'export {}');
    await writeFile(join(artifactRoot, 'dist/health.js'), 'export {}');
    await writeFile(join(candidateRoot, 'component.json'), JSON.stringify(manifest));
    const candidate: Host.Candidate = { candidateId: buildId, componentId: 'agent-manager', buildId, artifactRoot,
      manifestPath: join(candidateRoot, 'component.json'), createdAt: new Date().toISOString(),
      platform: { os: process.platform as 'win32' | 'linux', arch: process.arch as 'x64' | 'arm64', node: process.version } };
    let archive = await createPackageArchive(candidate, {});
    await assert.rejects(validatePackageArchive(archive.path, archive.hash, manifest, join(root, 'missing-extract'), {}),
      { code: 'artifact_missing' });
    if (process.platform === 'win32') await assert.rejects(verifyCandidate(candidate, { artifactRoot: root }), { code: 'artifact_missing' });
    await mkdir(join(artifactRoot, 'dist/native'));
    await writeFile(join(artifactRoot, 'dist/native/ivy-job.exe'), 'test launcher bytes');
    archive = await createPackageArchive(candidate, {});
    await validatePackageArchive(archive.path, archive.hash, manifest, join(root, 'complete-extract'), {});
    await verifyCandidate(candidate, { artifactRoot: root });
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
