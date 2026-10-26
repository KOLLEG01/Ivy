import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { fixture, fixtureToken } from './fixture.mjs';
import { nativeAgentFixture } from '../../fixtures/native-agent.mjs';
import { publishUi } from '../../../dist/packages/cli/src/publish-ui.js';
import { newOperationId } from '../../../dist/packages/sdk/src/client.js';

export async function agentFixture(t, viewport, completeCatalog = false, nativeVersion = '0.154.0', manageInstructions = false, serviceTasks = false) {
  const f = await fixture(t, false, viewport);
  const agent = await nativeAgentFixture(t, { ...f, credential: fixtureToken }, { completeCatalog, nativeVersion, manageInstructions, serviceTasks });
  const definition = JSON.parse(await readFile('ui/agent-ui/ui.json', 'utf8'));
  await publishUi(f.client, { directory: resolve('dist/apps/agent-ui'), definition, mutationId: await newOperationId(f.client, 'agent-publication-' + randomUUID()), expectedReleaseId: null });
  const url = f.base + '/ui/agent-ui/';
  const open = async hash => { await f.login(); await f.page.goto(url + (hash ?? '#/home')); };
  return { ...agent, url, open };
}
