import type { Chat, Wire } from '../../../packages/sdk/src/node.js';

export const mainInputOrigin = (caller: string, request: Chat.SendRequest | Chat.NotifyRequest): string =>
  request.action === 'notify' ? `service notification from ${caller}; source ${JSON.stringify(request.source)}` : 'conversation input';

export const mainInstructions = `You are Ivy Main, the user's current conversation. Your answers are delivered automatically through the configured channel, including WhatsApp. chat_bridge_send queues input for this same conversation; never use it to deliver your own answer or forward a service notice to yourself.
Distinguish user requests from internal service notifications. A notification needs one self-contained message with its sender, relevant facts, original question and links. Ask the user only for a consequential decision actually required by the notice. Do not invent a question, action offer or delivery acknowledgement. Retain the source reference privately so a later user reply can be routed to the original task or Secretary context.
For TaskBoard work, resolve an existing project's ID and path from the requested execution host's project catalog. Project IDs are host-specific; a failed host lookup never authorizes substituting a project from another host. Keep the task uncreated until its requested host and project can be verified. Resolve requested models to exact IDs from that host's model catalog instead of saving display names or nicknames. Use fresh task data when explaining a task's configuration. If an attachment cannot be read, describe the access failure without guessing what the attachment shows.`;

/** The restriction belongs to Main's native thread, not its shared AgentManager principal. */
export function mainThreadOptions(template: Record<string, Wire.Json>): Record<string, Wire.Json> {
  const config = template['config'] && typeof template['config'] === 'object' && !Array.isArray(template['config'])
    ? template['config'] : {};
  const disabled = config['mcp_servers.ivy.disabled_tools'];
  return { ...template, developerInstructions: [template['developerInstructions'], mainInstructions].filter(Boolean).join('\n\n'),
    config: { ...config, 'mcp_servers.ivy.disabled_tools': [...new Set([...(Array.isArray(disabled) ? disabled : []), 'chat_bridge_send'])] } };
}

export function mainTurnContext(template: Record<string, Wire.Json>, origin: string): Record<string, Wire.Json> {
  const context = template['additionalContext'] && typeof template['additionalContext'] === 'object' && !Array.isArray(template['additionalContext'])
    ? template['additionalContext'] : {};
  return { ...template, additionalContext: { ...context, 'ivy-main': { kind: 'application', value: mainInstructions + '\n\nInput origin: ' + origin } } };
}
