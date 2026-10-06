import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { expect } from '@playwright/test';
import { agentFixture } from './fixtures/agent-fixture.mjs';

test('AgentUI edits Hive-wide MCP and skills while AgentManager injects only its local credential', { timeout: 120000 }, async t => {
  const f = await agentFixture(t, { width: 1280, height: 1000 }, false, '0.154.0', true);
  await f.open();
  await f.page.getByRole('link', { name: 'Settings', exact: true }).click();
  await f.page.getByRole('link', { name: 'Hive settings', exact: true }).click();
  const environment = f.page.getByRole('region', { name: 'Hive agent environment', exact: true });
  const mcp = environment.getByRole('region', { name: 'MCP configuration', exact: true });
  const skills = environment.getByRole('region', { name: 'Skills configuration', exact: true });
  await expect(mcp.getByRole('button', { name: 'Save', exact: true })).toBeEnabled();
  await expect(mcp.getByLabel('Server definitions (JSON)')).toHaveValue(/ivy/);

  const servers = [{ name: 'ivy', url: f.base + '/mcp', enabled: true, authentication: 'agent-manager', startupTimeoutSeconds: 20, toolTimeoutSeconds: 60 }];
  await mcp.getByLabel('Server definitions (JSON)').fill(JSON.stringify(servers, null, 2));
  await mcp.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(mcp.getByRole('status')).toContainText('Saved revision 1');
  const configPath = join(f.config.settings.nativeHome, 'config.toml');
  await expect.poll(() => readFile(configPath, 'utf8').catch(() => ''), { timeout: 25000 }).toContain('startup_timeout_sec = 20');
  const localConfig = await readFile(configPath, 'utf8');
  assert.match(localConfig, /Authorization = "Bearer [^"]+"/);
  const storedMcp = await f.client.request('objects.read', { objectId: (await f.client.request('objects.stat', { path: '/ivy-agent-mcp' })).id });
  assert.doesNotMatch(JSON.stringify(storedMcp.content.value), /Bearer|credential/i);

  const files = [{ path: 'example/SKILL.md', content: '---\nname: example\ndescription: Managed fixture.\n---\n\n# Example\n' }];
  await expect(skills.getByRole('button', { name: 'Save', exact: true })).toBeEnabled();
  await skills.getByLabel('Files (JSON)').fill(JSON.stringify(files, null, 2));
  await skills.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(skills.getByRole('status')).toContainText('Saved revision 1');
  const skillPath = join(f.config.settings.skillsRoot, 'example', 'SKILL.md');
  await expect.poll(() => readFile(skillPath, 'utf8').catch(() => ''), { timeout: 25000 }).toContain('# Example');
});
