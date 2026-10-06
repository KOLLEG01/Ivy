import type { Wire } from '../../../packages/sdk/src/node.js';
import catalog from '../../../specs/schemas/chat.operations.json' with { type: 'json' };
import { bundledSchema } from '../../../packages/sdk/src/node.js';
import { chatContracts, readableVersions } from './schema.js';
import { chatInterfaceVersion } from '../../../packages/sdk/src/node.js';

const descriptions: Record<keyof typeof catalog.operations, string> = {
  retryResult: 'Retry reading the result of the original turn after a failed collection; does not start another turn.',
  workspace: 'Check the WhatsApp transport and the shared Main conversation binding.',
  createMain: 'Create or replace the shared Main conversation at its observed revision.',
  send: 'Queue input for Main to process. This does not send the supplied text directly to WhatsApp. Main must answer normally and must never use this tool to send its own answer or forward a notice to itself. Set expectedBinding=null only when creating the first Main.',
  cancel: 'Request cancellation of a queued or running input at its current revision.',
  receive: 'Offer saved Main replies to the WhatsApp transport.',
  acknowledge: 'Mark the exact offered replies as delivered after WhatsApp confirms them.',
  operation: 'Check the original outcome of a ChatBridge action by operationId.',
  history: 'Read message history for a configured channel without marking replies as delivered.',
  input: 'Check whether a message input is queued, running or finished in its channel.',
  notify: 'Give Main a service notification for the user. Main answers through its single WhatsApp transport; requires an authenticated producer.',
  notice: 'Read the Main reply and WhatsApp delivery state for this producer\'s original notification operation.',
};
const keywords: Partial<Record<keyof typeof catalog.operations, string[]>> = {
  workspace: ['WhatsApp'], history: ['WhatsApp'], send: ['WhatsApp', 'message'],
  notify: ['WhatsApp', 'send', 'message', 'notification'], notice: ['notification'],
};
export type ChatTool = keyof typeof catalog.operations;
function chatMcp(name: ChatTool): { name: string; surface: 'ivy' | 'ivy_dev' } | null {
  if (['receive', 'acknowledge'].includes(name)) return null;
  if (['send', 'notify'].includes(name)) return { name: 'chat_bridge_send', surface: 'ivy' };
  if (['history', 'input', 'notice', 'operation'].includes(name)) return { name: 'chat_bridge_read', surface: 'ivy' };
  if (name === 'workspace') return { name: 'chat_bridge_status', surface: 'ivy_dev' };
  return { name: 'chat_bridge_' + name.replace(/[A-Z]/g, letter => '_' + letter.toLowerCase()), surface: 'ivy_dev' };
}
export const chatTools: Wire.ToolDefinition[] = Object.entries(catalog.operations).map(([name, operation]) => ({
  namespace: 'chat', name, interfaceVersion: chatInterfaceVersion, description: descriptions[name as ChatTool],
  discovery: { ...(chatMcp(name as ChatTool) ? { mcp: chatMcp(name as ChatTool)! } : {}), keywords: keywords[name as ChatTool] ?? [] },
  inputSchema: bundledSchema(operation.input, catalog.schema), outputSchema: bundledSchema(operation.output, catalog.schema),
  annotations: { readOnlyHint: !operation.mutation, idempotentHint: true },
}));
export function chatRegistry(): Wire.RegistrySync {
  const contracts = chatContracts();
  return { discoveryHint: 'ChatBridge lets agents exchange messages with the user through WhatsApp via the shared Main conversation.', namespaces: [{ namespace: 'chat', description: 'Messages and service notifications through the shared Main conversation',
    guideMarkdown: 'Use chat_bridge_send to give Main user input or authenticated service notifications. It queues an input; it is not direct WhatsApp delivery. Main answers normally, and the configured channel automatically delivers that answer. Main must never call chat_bridge_send to deliver its own answer or forward a notice to itself. '
      + 'If no Main exists, send with expectedBinding=null. Keep operationId and messageId for the same message. A successful send means queued, not delivered; use chat_bridge_read for the original operation, input, notice or channel history after an uncertain reply. '
      + 'Service notifications supply only operationId, source and text; Main selects WhatsApp. Native pending inputs belong to the owning AgentManager.',
    tools: chatTools, topics: [], inventoryKinds: [] }], contracts,
    requiredContracts: [...new Set(contracts.map(c=>c.key))].map(key => {const versions=contracts.filter(c=>c.key===key).map(c=>c.version);return {key,
      readVersions: ['chat-bridge/conversation', 'chat-bridge/input', 'chat-bridge/result', 'chat-bridge/reply'].includes(key) ? [...readableVersions] : versions,
      writeVersions:[versions.at(-1)!]};}) };
}
