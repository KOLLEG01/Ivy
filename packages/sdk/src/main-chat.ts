import { callBound, discover, requireThat } from './client.js';
import type { Chat, RpcClient } from './client.js';

export const chatInterfaceVersion = '1.2.0';
export interface MainChat { serviceNodeId: string; expectedBridge: Chat.ExpectedBridge; channel: Chat.Channel }

/** Hive selects the unique ChatBridge. Consumers never configure their own Main destination. */
export async function mainChat(client: RpcClient): Promise<MainChat> {
  const bound = await discover(client, 'chat.workspace', { interfaceVersion: chatInterfaceVersion });
  const node = await client.request('serviceNodes.get', { serviceNodeId: bound.serviceNodeId });
  requireThat(node.serviceName === 'chat-bridge' && node.connected && node.synced && node.ready,
    'chat_main_unavailable', 'The Main ChatBridge is unavailable.');
  const workspace = await callBound(client, bound, {}) as unknown as Chat.WorkspaceInfo;
  requireThat(workspace.expectedBridge.principalId === node.principalId && workspace.nativeOwnerReady &&
    workspace.channels.length === 1 && workspace.channels[0]!.channel.adapter === 'whatsapp',
    'chat_main_unavailable', 'Main requires its single WhatsApp transport and native owner.');
  return { serviceNodeId: node.serviceNodeId, expectedBridge: workspace.expectedBridge,
    channel: workspace.channels[0]!.channel };
}

/** Transport metadata stays in durable receipts; callers submit only the original service content. */
export const mainNotification = ({ action, operationId, source, text }: Chat.NotifyRequest): Chat.NotifyRequest =>
  ({ action, operationId, source, text });
