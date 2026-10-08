import { readFile, mkdir } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { fixture, fixtureToken } from './fixture.mjs';
import { agentFixture } from './agent-fixture.mjs';
import { startTaskBoard } from '../../../dist/services/task-board/src/main.js';
import { atomicJson } from '../../../dist/packages/host-runtime/src/config.js';
import { publishUi } from '../../../dist/packages/cli/src/publish-ui.js';
import { discover, callBound, newOperationId } from '../../../dist/packages/sdk/src/client.js';

export async function taskBoardFixture(t, { native = false, viewport, scheduler = false } = {}) {
  const operationId = label => newOperationId(f.client, label + '-' + randomUUID());
  const f = native ? await agentFixture(t, viewport, true, '0.154.0', false, true) : await fixture(t, false, viewport);
  const build = JSON.parse(await readFile('dist/build-info.json', 'utf8'));
  const config = { schemaVersion: 1, componentId: 'task-board', instanceId: 'task-board', serviceNodeId: 'browser-task-board', hostId: 'isolated-task-board-host',
    publicBaseUrl: f.base, artifactRoot: join(f.root, 'task-board-artifact'), dataRoot: join(f.root, 'task-board'), buildId: build.buildId, version: build.version, credential: fixtureToken,
    settings: { principalId: 'browser-fixture', rootObjectId: null, scheduler: { enabled: scheduler, intervalMs: 1000, pageSize: 10 },  phoneTarget: null } };
  await mkdir(join(config.artifactRoot, 'dist'), { recursive: true }); await atomicJson(join(config.artifactRoot, 'dist/build-info.json'), build);
  const path = join(f.root, 'task-board.json'); await atomicJson(path, config);
  let service = await startTaskBoard(path); t.after(async () => service.close()); await service.service.waitReady();
  const definition = JSON.parse(await readFile('ui/task-board-ui/ui.json', 'utf8'));
  await publishUi(f.client, { directory: resolve('dist/apps/task-board-ui'), definition, mutationId: await operationId('task-board-publication'), expectedReleaseId: null });
  const url = f.base + '/ui/task-board-ui/';
  const open = async (hash = '#/home?node=browser-task-board') => { await f.login(); await f.page.goto(url + hash); };
  const invoke = async request => { const value = { operationId: await operationId('invoke'), ...request }; return callBound(f.client, await discover(f.client, 'task-board.' + value.action, { serviceNodeId: 'browser-task-board' }), value, value.operationId); };
  if (native) {
    const current = await callBound(f.client, await discover(f.client, 'task-board.configuration', { serviceNodeId: 'browser-task-board' }), {});
    await invoke({ action: 'configure', configuration: current.object, value: { ...current.configuration,
      defaults: { ...current.configuration.defaults, nativeOptions: { model: null, reasoningEffort: null, serviceTier: 'standard' } } } });
  }
  const task = async id => (await f.client.request('objects.read', { objectId: id })).content.value;
  const restartTaskBoard = async () => { await service.close(); service = await startTaskBoard(path); await service.service.waitReady(); };
  return { ...f, url, open, invoke, task, restartTaskBoard, taskBoard: () => service, taskBoardConfig: config };
}
