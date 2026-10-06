import { canonical, hashJson } from '../../../packages/sdk/src/node.js';
import { need, validate } from './schema.js';
import type { Message, SourceDefinition } from './schema.js';
import { validateMicrosoft } from './microsoft-schema.js';
import type { TeamsCollector, TeamsPage, TeamsSelection } from './teams-schema.js';

function check(condition: unknown, code = 'teams_response_invalid'): asserts condition { need(condition, code, 'Teams requires original account-bound complete chat and message evidence.'); }
const record = (value: unknown): Record<string, unknown> => { check(value && typeof value === 'object' && !Array.isArray(value)); return value as Record<string, unknown>; };
const text = (value: unknown, maximum: number, empty = false): string => {
  check(typeof value === 'string' && value.length <= maximum && (empty || value.trim().length > 0) && value.isWellFormed() && !/[\u0000-\u0008\u000b\u000c\u000e-\u001f\ufffe\uffff]/u.test(value)); return value;
};
export const teamsTime = (value: unknown): string => {
  const raw = text(value, 40), match = /^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d{1,7}))?Z$/.exec(raw);
  check(match && !(match[2] ?? '').slice(3).replaceAll('0', ''), 'teams_time_invalid');
  const result = match[1] + '.' + (match[2] ?? '').padEnd(3, '0').slice(0, 3) + 'Z';
  check(Number.isFinite(Date.parse(result)) && new Date(result).toISOString() === result, 'teams_time_invalid'); return result;
};
export function teamsArguments(configuration: TeamsCollector, selection: TeamsSelection): Record<string, string | number | boolean> {
  validateMicrosoft('TeamsCollector', configuration); validateMicrosoft('TeamsSelection', selection);
  check(teamsTime(selection.since) === selection.since && teamsTime(selection.until) === selection.until && selection.since < selection.until && selection.since === configuration.since, 'teams_selection_invalid');
  return selection.chat ? { chat_id: selection.chat.id, sent_after: selection.since, top: configuration.maximumMessages + 1 } : { unread_only: true, top: configuration.maximumChats + 1 };
}
export function teamsSource(configuration: TeamsCollector, source: SourceDefinition): void {
  check(source.kind === 'teams' && source.accountId === configuration.profile.id, 'microsoft_account_mismatch');
  const ids = [...source.ownSenderIds, ...(source.allowedSenderIds ?? [])];
  check(ids.every(id => /^(user|application):[^\s:]+$/.test(id)), 'teams_sender_config_invalid');
}
export function teamsPage(value: unknown, configuration: TeamsCollector, source: SourceDefinition, selection: TeamsSelection, observedAt: string): TeamsPage {
  teamsArguments(configuration, selection); validate('SourceDefinition', source); teamsSource(configuration, source);
  check(teamsTime(observedAt) === observedAt && observedAt >= selection.until, 'teams_time_invalid'); canonical(value, 8 * 1024 * 1024);
  const result: TeamsPage = { schemaVersion: 1, sourceId: source.sourceId, accountId: source.accountId, observedAt, selection: structuredClone(selection), chats: [], messages: [],
    excluded: { hiddenChat: 0, configuredChat: 0, ownSender: 0, senderNotAllowed: 0, deleted: 0, system: 0, outsideWindow: 0 }, messagesWithAttachments: 0 };
  const page = record(value), entries = page[selection.chat ? 'messages' : 'chats'], maximum = selection.chat ? configuration.maximumMessages : configuration.maximumChats;
  check(Array.isArray(entries) && entries.length <= maximum + 1, 'teams_page_incomplete'); check(entries.length <= maximum, 'teams_scan_limit');
  // The selected connector performs pagination itself; an exposed continuation is never ignored.
  check(!page['next_link'] && !page['nextLink'] && !page['@odata.nextLink'] && !page['has_more'] && !page['next_from_index'], 'teams_page_incomplete');
  const seen = new Set<string>(), own = new Set(['user:' + configuration.profile.id, ...source.ownSenderIds]);
  for (const entry of entries) {
    const raw = record(entry), id = text(raw[selection.chat ? 'message_id' : 'id'], 4096);
    check(!seen.has(id), 'teams_duplicate_id'); seen.add(id);
    if (!selection.chat) {
      check(raw['is_unread'] === true && typeof raw['is_hidden'] === 'boolean', 'teams_selection_mismatch');
      if (configuration.excludedChatIds.includes(id)) { result.excluded.configuredChat++; continue; }
      if (raw['is_hidden']) { result.excluded.hiddenChat++; continue; }
      const title = raw['topic'] === null ? '' : text(raw['topic'], 1024, true);
      result.chats.push({ id, title }); continue;
    }
    check(raw['chat_id'] === selection.chat.id && raw['container_type'] === 'chat' && raw['channel_id'] === null && raw['team_id'] === null && raw['parent_message_id'] === null &&
      raw['path'] === '/chats/' + selection.chat.id + '/messages/' + id, 'teams_message_identity_invalid');
    if (raw['deleted_at'] !== null) { teamsTime(raw['deleted_at']); result.excluded.deleted++; continue; }
    if (raw['message_type'] !== 'message') { check(raw['message_type'] === 'systemEventMessage', 'teams_message_type_invalid'); result.excluded.system++; continue; }
    const user = raw['author_user_id'], application = raw['author_application_id'];
    check((user === null) !== (application === null), 'teams_sender_invalid');
    const senderId = (user === null ? 'application:' : 'user:') + text(user === null ? application : user, 256);
    check(/^(user|application):[^\s:]+$/.test(senderId), 'teams_sender_invalid');
    if (own.has(senderId)) { result.excluded.ownSender++; continue; }
    if (source.allowedSenderIds !== null && !source.allowedSenderIds.includes(senderId)) { result.excluded.senderNotAllowed++; continue; }
    const occurredAt = teamsTime(raw['created_at']); check(occurredAt >= selection.since, 'teams_selection_mismatch');
    if (occurredAt >= selection.until) { result.excluded.outsideWindow++; continue; }
    const body = text(raw['content'], 32768, true), title = text(raw['title'], 1024, true), url = text(raw['web_link'], 4096);
    let link: URL; try { link = new URL(url); } catch { check(false, 'teams_url_invalid'); }
    check(link.protocol === 'https:' && !link.username && !link.password, 'teams_url_invalid');
    check(typeof raw['has_attachments'] === 'boolean' && Array.isArray(raw['mentions']) && raw['mentions'].length <= 100, 'teams_body_invalid');
    if (raw['has_attachments']) result.messagesWithAttachments++;
    const message: Message = { accountId: source.accountId, conversationId: hashJson(['teams-chat', selection.chat.id]), messageId: hashJson(['teams-message', selection.chat.id, id]),
      // The real connector reports false for hosted AMS images. Plain-text projection cannot
      // establish absence of media, so every native Teams capture requires separate coverage.
      senderId, outgoing: false, attachments: 'expected', occurredAt, observedAt, title, text: body, url: link.href,
      nativeRevision: 'teams-content:' + hashJson({ chatId: selection.chat.id, id, senderId, occurredAt, title, text: body, hasAttachments: raw['has_attachments'], mentions: raw['mentions'], mediaVerificationRequired: true }).slice(7) };
    validate('Message', message); result.messages.push(message);
  }
  canonical(result, 192 * 1024); validateMicrosoft('TeamsPage', result); return result;
}
