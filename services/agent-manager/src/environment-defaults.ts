import defaults from './environment-defaults.json' with { type: 'json' };

import { validateAgent } from '../../../packages/sdk/src/node.js';
import type { Agent } from '../../../packages/sdk/src/node.js';

/** Project-owned generated defaults used until AgentUI stores an explicit Hive-wide document. */
export async function loadEnvironmentDefaults(
  _artifactRoot: string,
  publicBaseUrl: string,
): Promise<Agent.EnvironmentDefaults> {
  const value = structuredClone(defaults) as unknown as Agent.EnvironmentDefaults;
  const base = new URL(publicBaseUrl);
  for (const server of value.mcp.servers)
    server.url = new URL(
      base.pathname.replace(/\/$/, '') + (server.name === 'ivy_dev' ? '/mcp-dev' : '/mcp'),
      base.origin,
    ).href;
  validateAgent('EnvironmentDefaults', value);
  return value;
}
