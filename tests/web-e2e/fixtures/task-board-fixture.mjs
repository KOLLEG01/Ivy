import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { fixture, fixtureToken } from './fixture.mjs';
import { agentFixture } from './agent-fixture.mjs';
import { catalogArtifact } from '../../fixtures/native-artifact.mjs';
import { startNativeService } from '../../../tools/testing/native-service.mjs';
import { publishUi } from '../../../dist/packages/cli/src/publish-ui.js';
import { discover, callBound, newOperationId } from '../../../dist/packages/sdk/src/client.js';

export async function taskBoardFixture(t, { native = false, viewport, scheduler = false } = {}) {
  const operationId = label => newOperationId(f.client, label + '-' + randomUUID());
  const f = native ? await agentFixture(t, viewport, true, '0.154.0', false, true) : await fixture(t, false, viewport);
  const config = { schemaVersion: 1, componentId: 'task-board', instanceId: 'task-board', serviceNodeId: 'browser-task-board', hostId: 'isolated-task-board-host',
    publicBaseUrl: f.base, dataRoot: join(f.root, 'task-board'), credential: fixtureToken,
    settings: { principalId: 'browser-fixture', rootObjectId: null, scheduler: { enabled: scheduler, intervalMs: 1000, pageSize: 10 },  phoneTarget: null } };
  const artifact = native ? await catalogArtifact('task-board', f.root, f.catalog) : undefined;
  let service = await startNativeService(config, { artifact }); t.after(async () => service.close());
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
  const task = async id => {
    const tool = await discover(f.client, 'task-board.read', { serviceNodeId: 'browser-task-board' });
    let page = await callBound(f.client, tool, { task: id, comments: { limit: 100 } });
    const value = page.task, comments = [...value.comments];
    while (page.commentPage.nextBefore !== null) {
      page = await callBound(f.client, tool, { task: id, comments: { limit: 100, before: page.commentPage.nextBefore } });
      comments.unshift(...page.task.comments);
    }
    return { ...value, comments: comments.sort((a, b) => a.sequence - b.sequence) };
  };
  const patchTask = async (pin, change) => {
    const id = typeof pin === 'string' ? pin : pin.objectId;
    const current = await f.client.request('objects.read', { objectId: id });
    return f.client.request('objects.write', { mutationId: await operationId('fixture-task-patch'), objectId: id,
      expectedRevision: current.revision.revision, contractVersion: current.revision.contractVersion, references: current.revision.references,
      content: { encoding: 'json', value: { ...current.content.value, ...change } } });
  };
  const seedComments = async (id, comments) => {
    let revision = (await f.client.request('objects.read', { objectId: id })).revision.revision;
    for (const comment of comments) {
      const outcome = await invoke({ action: 'comment', taskId: id, expectedRevision: revision,
        purpose: 'update', ...(comment.sourceTaskId ? { agentDelivery: 'queue' } : {}), ...comment });
      revision = outcome.task.revision;
    }
  };
  const restartTaskBoard = async () => { await service.close(); service = await startNativeService(config, { artifact }); };
  return { ...f, url, open, invoke, task, patchTask, seedComments, restartTaskBoard, taskBoardConfig: config };
}
