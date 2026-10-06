import { canonical, hashJson, IvyError } from '../../../packages/sdk/src/node.js';
import { parse } from 'parse5';
import type { DefaultTreeAdapterMap } from 'parse5';
import { need, validate } from './schema.js';
import type { Message, SourceDefinition } from './schema.js';
import { validateMicrosoft } from './microsoft-schema.js';
import type { MicrosoftProfile, OutlookPage, OutlookSelection } from './microsoft-schema.js';

function check(condition: unknown, code = 'microsoft_response_invalid'): asserts condition { need(condition, code, 'Microsoft source requires complete account-bound original evidence.'); }
const record = (value: unknown): Record<string, unknown> => { check(value && typeof value === 'object' && !Array.isArray(value)); return value as Record<string, unknown>; };
const text = (value: unknown, maximum: number, empty = false): string => {
  check(typeof value === 'string' && value.length <= maximum && (empty || value.trim().length > 0) && !/[\u0000-\u0008\u000b\u000c\u000e-\u001f\ud800-\udfff\ufffe\uffff]/u.test(value)); return value;
};
const email = (value: unknown): string => { const result = text(value, 256); check(/^[^\s<>@]+@[^\s<>@]+$/.test(result)); return result.toLowerCase(); };
const utc = (value: unknown): string => {
  const raw = text(value, 40), match = /^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d)(?:\.(\d{1,7}))?Z$/.exec(raw);
  check(match && !(match[2] ?? '').slice(3).replaceAll('0', ''), 'microsoft_time_invalid');
  const result = match[1] + '.' + (match[2] ?? '').padEnd(3, '0').slice(0, 3) + 'Z';
  check(Number.isFinite(Date.parse(result)) && new Date(result).toISOString() === result, 'microsoft_time_invalid'); return result;
};
export function microsoftProfile(value: unknown): MicrosoftProfile {
  const source = record(value), profile = { id: text(source['id'], 256), email: text(source['email'], 256) }; email(profile.email);
  validateMicrosoft('MicrosoftProfile', profile); return profile;
}
export function microsoftMcp(value: unknown): Record<string, unknown> {
  const result = record(value); check(Array.isArray(result['content']));
  if (result['isError'] === true) {
    const failure = record(result['structuredContent']);
    if (failure['error_code'] === 'RATE_LIMITED') {
      const seconds = failure['retry_after_seconds']; check(Number.isSafeInteger(seconds) && Number(seconds) >= 1 && Number(seconds) <= 86400, 'microsoft_retry_invalid');
      throw new IvyError('microsoft_rate_limited', 'Microsoft source must wait before a later new attempt.', 'completed', { retryAfterSeconds: Number(seconds) });
    }
    throw new IvyError('microsoft_provider_failed', 'Microsoft rejected the original read.', 'completed');
  }
  check(result['isError'] === false || result['isError'] === null || result['isError'] === undefined); return record(result['structuredContent']);
}
function selection(value: OutlookSelection): void {
  validateMicrosoft('OutlookSelection', value); check(utc(value.since) === value.since && utc(value.until) === value.until && value.since < value.until, 'microsoft_selection_invalid');
}
export function outlookArguments(value: OutlookSelection) {
  selection(value); return { folder_id: 'inbox', top: value.pageSize, skip: value.skip,
    filter: 'receivedDateTime ge ' + value.since + ' and receivedDateTime lt ' + value.until + ' and isRead eq false', order_by: 'receivedDateTime desc' };
}
const blocks = new Set(['address', 'article', 'aside', 'blockquote', 'div', 'dl', 'dt', 'dd', 'footer', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'header', 'hr', 'li', 'main', 'ol', 'p', 'pre', 'section', 'table', 'tr', 'ul']);
const inert = new Set(['script', 'style', 'template', 'head']);
type HtmlNode = DefaultTreeAdapterMap['node'];
/** Preserve image references before reducing HTML; never fetch these untrusted URLs. */
export function outlookInlineReferences(value: unknown): string[] {
  const body = record(value); if (body['contentType'] !== 'html') return [];
  const pending: HtmlNode[] = [parse(text(body['content'], 1024 * 1024, true))], references: string[] = []; let visited = 0;
  while (pending.length) {
    const node = pending.pop()!; check(++visited <= 100000, 'microsoft_body_too_large');
    if ('tagName' in node) {
      if (inert.has(node.tagName)) continue;
      if (['img', 'image', 'object', 'video', 'source'].includes(node.tagName)) {
        references.push(hashJson({ tag: node.tagName, attrs: node.attrs }));
        check(references.length <= 1024, 'microsoft_body_too_large');
      }
    }
    if ('childNodes' in node) pending.push(...node.childNodes);
  }
  return references.sort();
}
export function outlookHasMedia(raw: Record<string, unknown>): boolean {
  return raw['has_attachments'] === true || outlookInlineReferences(raw['body']).length > 0;
}
export function outlookBody(value: unknown): string {
  const body = record(value), content = text(body['content'], 1024 * 1024, true); check(body['contentType'] === 'text' || body['contentType'] === 'html', 'microsoft_body_invalid');
  if (body['contentType'] === 'text') return text(content, 32768, true);
  const parts: string[] = [], pending: (HtmlNode | string)[] = [parse(content)]; let nodes = 0;
  while (pending.length) {
    const node = pending.pop()!; if (typeof node === 'string') { parts.push(node); continue; }
    check(++nodes <= 100000, 'microsoft_body_too_large');
    if ('value' in node) { parts.push(node.value); continue; }
    if ('tagName' in node) {
      if (inert.has(node.tagName)) continue;
      if (node.tagName === 'br' || blocks.has(node.tagName)) parts.push('\n');
      if (blocks.has(node.tagName)) pending.push('\n');
      if (node.tagName === 'td' || node.tagName === 'th') { parts.push(' '); pending.push(' '); }
      if (node.tagName === 'a') {
        const href = node.attrs.find(attr => attr.name === 'href')?.value;
        if (href) {
          let url: URL | null = null; try { url = new URL(href); } catch { /* Invalid links are inert text only. */ }
          if (url && ['https:', 'http:', 'mailto:'].includes(url.protocol) && !url.username && !url.password) pending.push(' (' + text(url.href, 4096) + ')');
        }
      }
      if (node.tagName === 'img') { const alt = node.attrs.find(attr => attr.name === 'alt')?.value; if (alt) parts.push(' [' + alt + '] '); }
    }
    if ('childNodes' in node) for (let index = node.childNodes.length - 1; index >= 0; index--) pending.push(node.childNodes[index]!);
  }
  return text(parts.join('').replace(/[\t\r ]+/g, ' ').replace(/ *\n */g, '\n').replace(/\n{3,}/g, '\n\n').trim(), 32768, true);
}
export function outlookPage(value: unknown, profile: MicrosoftProfile, source: SourceDefinition, selected: OutlookSelection, observedAt: string): OutlookPage {
  validateMicrosoft('MicrosoftProfile', profile); validate('SourceDefinition', source); selection(selected);
  check(source.kind === 'email' && source.accountId === profile.id, 'microsoft_account_mismatch');
  check(source.ownSenderIds.every(id => email(id) === id) && (source.allowedSenderIds === null || source.allowedSenderIds.every(id => email(id) === id)), 'microsoft_sender_config_invalid');
  check(utc(observedAt) === observedAt && observedAt >= selected.until, 'microsoft_time_invalid');
  canonical(value, 8 * 1024 * 1024); const page = record(value), entries = page['value'];
  check(Array.isArray(entries) && entries.length <= selected.pageSize && typeof page['has_more'] === 'boolean', 'microsoft_page_incomplete');
  const next = page['next_from_index'];
  if (page['has_more']) {
    check(entries.length > 0 && Number.isSafeInteger(next) && Number(next) > selected.skip && Number(next) <= 1000000 && typeof page['next_link'] === 'string', 'microsoft_page_incomplete');
    let url: URL; try { url = new URL(page['next_link']); } catch { check(false, 'microsoft_page_incomplete'); }
    check(url.protocol === 'https:' && url.hostname === 'graph.microsoft.com' && !url.username && !url.password, 'microsoft_page_incomplete');
  } else check(next === null && page['next_link'] === null, 'microsoft_page_incomplete');
  const result: OutlookPage = { schemaVersion: 1, sourceId: source.sourceId, accountId: profile.id, observedAt, selection: structuredClone(selected), nextIndex: page['has_more'] ? Number(next) : null,
    complete: !page['has_more'], messages: [], excluded: { ownSender: 0, senderNotAllowed: 0 }, messagesWithAttachments: 0 };
  const seen = new Set<string>(), own = new Set([email(profile.email), ...source.ownSenderIds]); let previousAt = selected.until;
  for (const entry of entries) {
    const raw = record(entry), sender = email(record(record(raw['sender'])['emailAddress'])['address']);
    if (own.has(sender)) { result.excluded.ownSender++; continue; }
    if (source.allowedSenderIds !== null && !source.allowedSenderIds.includes(sender)) { result.excluded.senderNotAllowed++; continue; }
    const providerId = text(raw['id'], 4096); check(!seen.has(providerId), 'microsoft_duplicate_id'); seen.add(providerId);
    const occurredAt = utc(raw['receivedDateTime']); check(occurredAt >= selected.since && occurredAt < selected.until && occurredAt <= previousAt && raw['isRead'] === false, 'microsoft_selection_mismatch'); previousAt = occurredAt;
    check(typeof raw['has_attachments'] === 'boolean'); const inlineReferences = outlookInlineReferences(raw['body']), hasMedia = raw['has_attachments'] || inlineReferences.length > 0;
    if (hasMedia) result.messagesWithAttachments++;
    const title = text(raw['subject'], 1024, true), body = outlookBody(raw['body']), url = text(raw['web_link'], 4096);
    let link: URL; try { link = new URL(url); } catch { check(false, 'microsoft_url_invalid'); }
    check(link.protocol === 'https:' && !link.username && !link.password && link.href === url, 'microsoft_url_invalid');
    const messageId = hashJson(['outlook-message', providerId]);
    const message: Message = { accountId: profile.id, conversationId: messageId, messageId, senderId: sender, outgoing: false, attachments: hasMedia ? 'expected' : 'none', occurredAt, observedAt, title, text: body, url,
      nativeRevision: (inlineReferences.length ? 'outlook-inline-content:' : 'outlook-content:') + hashJson({ projectionVersion: 2, providerId, sender, occurredAt, title, text: body, hasAttachments: raw['has_attachments'], inlineReferences }).slice(7) };
    validate('Message', message); result.messages.push(message);
  }
  canonical(result, 192 * 1024); validateMicrosoft('OutlookPage', result); return result;
}
