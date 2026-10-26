// Consume the normal native component build in the existing process fixture.
// Only the disposable native protocol peer's catalog pin differs from the build.
import { cp, readFile, writeFile, stat } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import assert from 'node:assert/strict';
import { materializeNativePlan } from '../../packages/host-runtime/src/native-plan.mjs';

const [root, repository] = process.argv.slice(2);
const source = resolve(process.env.IVY_TEST_AGENT_MANAGER_ARTIFACT ?? process.env.IVY_TEST_AGENT_ARTIFACT ?? repository);
const plan = materializeNativePlan(JSON.parse(await readFile(join(repository, 'services/agent-manager/deploy.json'), 'utf8')));
const stamp = JSON.parse(await readFile(join(source, 'dist/build-info.json'), 'utf8'));
assert.equal(stamp.componentId, 'agent-manager', 'Build the native Agent component before running its process fixture.');
assert.equal(stamp.version, plan.version);
const artifactRoot = join(root, 'prepared');
await cp(join(source, 'dist'), join(artifactRoot, 'dist'), { recursive: true, errorOnExist: true, force: false });
const catalogDirectory = 'dist/specs/native/codex-0.154.0';
const platformCatalog = join(catalogDirectory, `catalog.${process.platform}-${process.arch}.json`);
const catalog = await stat(join(artifactRoot, platformCatalog)).catch(() => null)
  ? platformCatalog : join(catalogDirectory, 'catalog.json');
const retained = JSON.parse(await readFile(join(artifactRoot, catalog), 'utf8'));
const peer = JSON.parse(await readFile(join(root, 'artifact', catalogDirectory, 'catalog.json'), 'utf8'));
retained.nativeExecutableHash = peer.nativeExecutableHash;
await writeFile(join(artifactRoot, catalog), JSON.stringify(retained));
const executable = join(artifactRoot, plan.entrypoint.executable);
const health = join(artifactRoot, plan.readiness.command.executable);
for (const path of [executable, health]) assert.ok((await stat(path)).isFile());
console.log(JSON.stringify({ artifactRoot, buildId: stamp.buildId, version: stamp.version, executable, health }));
