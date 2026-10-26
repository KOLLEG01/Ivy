import { createHash } from 'node:crypto';

const hash = value => createHash('sha256').update(JSON.stringify(value)).digest('hex');
const fail = code => Object.assign(new Error(code), { code });
const order = (a, b) => a.conversationId.localeCompare(b.conversationId) || a.id.localeCompare(b.id);
const bare = jid => String(jid).replace(/:[0-9]+@/, '@').replace(/@c\.us$/, '@s.whatsapp.net');
const jid = value => typeof value === 'string' && value.length <= 256 && /^[A-Za-z0-9.:_-]+@(s\.whatsapp\.net|lid|g\.us|c\.us)$/.test(value);

/** Direct MCP consumer; no provider credentials or secondary account connection. */
export default async function collect({ config, mcp, state, signal }) {
  if (!mcp || !jid(config.expectedAccountJid) || !config.expectedDatabaseId || !config.accountKey ||
      !Number.isSafeInteger(config.maximumPages) || config.maximumPages < 1 || config.maximumPages > 100 ||
      !Number.isSafeInteger(config.pageSize) || config.pageSize < 1 || config.pageSize > 25 ||
      !Number.isSafeInteger(config.maximumPending) || config.maximumPending < 1 || config.maximumPending > 100 ||
      !Number.isFinite(Date.parse(config.since))) throw fail('configuration_invalid');
  const key = 'whatsapp:' + config.accountKey;
  const previous = state ?? { cursor: 0, initialized: false, pending: {}, activity: [] };
  let next = structuredClone(previous);
  const accounts = {};
  let complete = false, issue = null, journalComplete = false, unknownUnread = 0;
  try {
    signal.throwIfAborted();
    const status = await mcp.call('whatsrust_status', {});
    if (!status?.ok || status.connected !== true) throw fail('source_disconnected');
    for (let page = 0; page < config.maximumPages; page++) {
      signal.throwIfAborted();
      const events = await mcp.call('whatsrust_events', { after: next.cursor, limit: config.pageSize, database_id: config.expectedDatabaseId });
      if (events.ok !== true || events.database_id !== config.expectedDatabaseId || events.account_jid !== config.expectedAccountJid)
        throw fail('source_identity_changed');
      if (events.gap === true) throw fail('event_journal_gap');
      if (!Array.isArray(events.items) || typeof events.has_more !== 'boolean' ||
          !Number.isSafeInteger(events.through_sequence) || events.through_sequence < next.cursor) throw fail('event_page_invalid');
      let through = next.cursor;
      for (const event of events.items) {
        if (!Number.isSafeInteger(event.sequence) || event.sequence <= through) throw fail('event_page_invalid');
        through = event.sequence;
        if (!next.initialized || !['message.received', 'message.updated'].includes(event.kind)) continue;
        const message = event.data;
        if (!message || typeof message.from_me !== 'boolean' || !jid(message.chat_jid)) throw fail('message_invalid');
        if (message.from_me) continue;
        if (!jid(message.sender_jid) ||
            typeof message.message_id !== 'string' || !message.message_id || message.message_id.length > 256 || typeof message.body !== 'string' ||
            typeof message.kind !== 'string' || message.kind.length > 80 || !Number.isSafeInteger(message.timestamp)) throw fail('message_invalid');
        if (bare(message.sender_jid) === bare(config.expectedAccountJid) ||
            (config.ownSenderIds ?? []).some(id => bare(id) === bare(message.sender_jid)) ||
            (config.excludedConversationIds ?? []).some(id => bare(id) === bare(message.chat_jid)) ||
            (config.allowedSenderIds?.length && !config.allowedSenderIds.some(id => bare(id) === bare(message.sender_jid)))) continue;
        const id = hash([message.chat_jid, message.message_id]);
        if (!next.pending[id] && message.timestamp * 1000 < Date.parse(config.since)) continue;
        next.pending[id] = {
          id: message.message_id, conversationId: message.chat_jid, senderId: message.sender_jid,
          revision: hash([message.kind, message.body, message.media_id]), occurredAt: new Date(message.timestamp * 1000).toISOString(),
          kind: message.kind, excerpt: message.body.slice(0, 500), mediaAvailable: !!message.media_id,
          reference: { server: config.server ?? 'whatsapp', historyTool: 'whatsrust_history', mediaTool: 'whatsrust_media', jid: message.chat_jid, messageId: message.message_id },
        };
        if (Object.keys(next.pending).length > config.maximumPending) throw fail('pending_limit');
      }
      if (through !== events.through_sequence || events.has_more && through === next.cursor) throw fail('event_page_invalid');
      next.cursor = through;
      if (!events.has_more) { journalComplete = true; break; }
    }
    if (!next.initialized && journalComplete) {
      next.initialized = true;
      next.baselineAt = new Date().toISOString();
    }
    if (!journalComplete) throw fail('event_page_limit');
    const unread = new Map();
    for (const conversationId of new Set(Object.values(next.pending).map(m => m.conversationId))) {
      signal.throwIfAborted();
      const value = await mcp.call('whatsrust_unread', { jid: conversationId });
      // `complete` describes the total count. A live unread flag can be known while that count is null.
      if (value?.ok !== true || value.connected !== true || typeof value.unread !== 'boolean' ||
          !Number.isSafeInteger(value.observed_at) || value.observed_at <= 0 || value.read_receipts_sent !== false) {
        unknownUnread++;
        unread.set(conversationId, null);
      } else unread.set(conversationId, value.unread);
    }
    for (const [id, message] of Object.entries(next.pending))
      if (unread.get(message.conversationId) === false) delete next.pending[id];
    if (unknownUnread) throw fail('unread_incomplete');
    next.activity = Object.values(next.pending).map(({ id, conversationId, revision, occurredAt }) => ({ id, conversationId, revision, occurredAt })).sort(order);
    complete = true;
  } catch (error) {
    signal.throwIfAborted();
    issue = /^[a-z][a-z0-9_]{0,63}$/.test(error?.code ?? '') ? error.code : 'provider_unavailable';
    // An invalid identity/gap/budget cannot move the durable cursor or lose earlier pending work.
    if (!['unread_incomplete', 'event_page_limit'].includes(issue)) next = structuredClone(previous);
  }
  const messages = Object.values(next.pending).sort(order);
  const observedAt = new Date().toISOString();
  accounts[key] = {
    application: 'whatsapp', displayName: config.displayName ?? 'WhatsApp', accountId: config.expectedAccountJid,
    ok: complete, status: complete ? 'ok' : 'trouble', state: complete ? 'healthy' : 'incomplete',
    unreadComplete: false, unreadCount: null,
    pendingUnreadComplete: complete, pendingUnreadCount: complete ? messages.length : null,
    pendingUnreadConversations: complete ? new Set(messages.map(m => m.conversationId)).size : null,
    observedAt, healthDetail: issue ?? '', messages: messages.slice(0, 50),
  };
  return {
    data: { schemaVersion: 1, accounts, activity: { [key]: next.activity ?? [] },
      observation: { complete, observedAt },
      source: { server: config.server ?? 'whatsapp', databaseId: config.expectedDatabaseId, cursor: next.cursor, journalComplete, baselineAt: next.baselineAt ?? null,
        coverage: 'live_events_since_baseline', unknownUnread, error: issue } },
    state: next,
  };
}
