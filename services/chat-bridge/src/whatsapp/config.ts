import { readFile } from 'node:fs/promises';
import { isAbsolute } from 'node:path';
import { requireThat } from '../../../../packages/sdk/src/node.js';
import type { Chat } from '../../../../packages/sdk/src/node.js';
export interface WhatsAppSender { phoneNumber: string; principalId: string; channelId: string }
export interface WhatsAppConfig {
  accountId: string; botPhoneNumber: string; senders: WhatsAppSender[]; since: string;
  heartbeatMs?: number;
  transcription?: { apiKey: string; baseUrl: string; model: string; language?: string };
}
export const phone = (value: string): string|null => {
  const match=/^(?:\+|00)?([1-9][0-9]{6,14})$/.exec(value); return match?.[1]??null;
};
export const jidPhone = (value: string): string|null => /^([1-9][0-9]{6,14})(?::\d+)?@(?:s\.whatsapp\.net|c\.us)$/.exec(value)?.[1]??null;
export async function readWhatsAppConfig(path: string, definition: Chat.Definition): Promise<WhatsAppConfig> {
  requireThat(isAbsolute(path),'invalid_arguments','WhatsApp configuration must use an absolute protected path.');
  const value=JSON.parse(await readFile(path,'utf8')) as WhatsAppConfig;
  requireThat(value && typeof value.accountId==='string' && !!phone(value.botPhoneNumber) && Array.isArray(value.senders) && value.senders.length===1 &&
    Number.isFinite(Date.parse(value.since)) && (value.heartbeatMs===undefined || Number.isInteger(value.heartbeatMs) && value.heartbeatMs>=10000 && value.heartbeatMs<=120000), 'invalid_arguments','Invalid WhatsApp account, sender policy or heartbeat.');
  const seen=new Set<string>(), principals=new Set<string>(),channels=new Set<string>();
  for(const sender of value.senders){
    const number=phone(sender.phoneNumber); requireThat(number && !seen.has(number) && !principals.has(sender.principalId) && !channels.has(sender.channelId),'invalid_arguments','WhatsApp requires unique exact phone senders, principals and channels.'); seen.add(number);principals.add(sender.principalId);channels.add(sender.channelId);
    requireThat(definition.channels.some(c=>c.channel.adapter==='whatsapp' && c.channel.accountId===value.accountId && c.channel.channelId===sender.channelId), 'chat_sender_refused','Every WhatsApp sender requires its exact configured channel.');
  }
  return value;
}
