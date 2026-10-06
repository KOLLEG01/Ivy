import { IvyError, serviceTools } from '../../../packages/sdk/src/node.js';
import type { Operation, RpcClient } from '../../../packages/sdk/src/node.js';
import type { Policy } from './schema.js';

export type VoiceTarget = NonNullable<Policy['voiceEscalation']>;

export function uniqueVoiceTarget(candidates: VoiceTarget[]): VoiceTarget | null {
  return candidates.length === 1 ? candidates[0]! : null;
}

/** A missing explicit target means auto-select one ready PhoneBridge with one recipient. */
export async function discoverVoiceTarget(client: RpcClient): Promise<VoiceTarget | null> {
  const nodes: Operation.ServiceNode[] = []; let cursor: string | undefined;
  do {
    const page = await client.request('serviceNodes.list', { serviceName: 'phone-bridge', limit: 200, ...(cursor ? { cursor } : {}) });
    nodes.push(...page.items); cursor = page.nextCursor ?? undefined;
  } while (cursor);
  const candidates: VoiceTarget[] = [];
  for (const node of nodes) {
    if (node.serviceName !== 'phone-bridge' || !node.connected || !node.synced || !node.ready || node.stale) continue;
    try {
      const tools = serviceTools(client, node.serviceNodeId, [{ namespace: 'phone', interfaceVersion: '1.0.0' }]);
      const status = await tools.call('phone.status', {}) as { recipients: string[]; registration: { state: string } | null };
      if (status.registration?.state !== 'registered' || !Array.isArray(status.recipients) || status.recipients.length !== 1) continue;
      const binding = await tools.binding('phone.request');
      candidates.push({ serviceNodeId: node.serviceNodeId, recipientId: status.recipients[0]!, requestDefinitionHash: binding.definitionHash });
    } catch (error) { if (!(error instanceof IvyError)) throw error; /* A changing owner is not an automatic default. */ }
  }
  return uniqueVoiceTarget(candidates);
}
