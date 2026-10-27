import { createHash } from 'node:crypto';

const hash = value => createHash('sha256').update(JSON.stringify(value)).digest('hex');
const fail = code => Object.assign(new Error(code), { code });
const order = (a, b) => a.conversationId.localeCompare(b.conversationId) || a.id.localeCompare(b.id);
const bare = jid => String(jid).replace(/:[0-9]+@/, '@').replace(/@c\.us$/, '@s.whatsapp.net');
const jid = value => typeof value === 'string' && value.length <= 256 && /^[A-Za-z0-9.:_-]+@(s\.whatsapp\.net|lid|g\.us|c\.us)$/.test(value);

async function unreadSummary(config, mcp, signal, pending) {
  const excluded = new Set([config.expectedAccountJid, ...(config.ownSenderIds ?? []),
    ...(config.excludedConversationIds ?? []), '0@c.us'].map(bare));
  const seen = new Set(), cursors = new Set();
  let cursor, count = 0, unknown = 0, total = 0, lookups = 0, oldest = Infinity;
  for (let page = 0; page < 20; page++) {
    signal.throwIfAborted();
    const result = await mcp.call('whatsrust_chats', { limit: 200, ...(cursor ? { cursor } : {}) });
    if (!Array.isArray(result.items) || result.coverage !== 'locally_synchronized' ||
        !(result.next_cursor === null || typeof result.next_cursor === 'string' && result.next_cursor.length > 0)) throw fail('unread_page_invalid');
    for (const chat of result.items) {
      if (!jid(chat.jid) || excluded.has(bare(chat.jid))) continue;
      const identity = bare(chat.jid);
      if (seen.has(identity)) throw fail('unread_duplicate_chat');
      seen.add(identity);
      // Keep archived identities seen so pending activity cannot add them back.
      if (chat.archived === true) continue;
      // An allow-list cannot establish which sender caused a group's unread flag.
      if (config.allowedSenderIds?.length && !config.allowedSenderIds.some(id => bare(id) === identity)) continue;
      total++;
      let flag = Number.isSafeInteger(chat.unread_count) && chat.unread_count >= 0 &&
        Number.isSafeInteger(chat.unread_observed_at) && chat.unread_observed_at > 0 ? chat.unread_count > 0 : null;
      let observedAt = chat.unread_observed_at;
      if (flag === null && lookups < 100) {
        lookups++;
        const value = await mcp.call('whatsrust_unread', { jid: chat.jid });
        if (value?.ok !== true || value.connected !== true || value.read_receipts_sent !== false) throw fail('unread_source_unavailable');
        if (typeof value.unread === 'boolean' && Number.isSafeInteger(value.observed_at) && value.observed_at > 0) {
          flag = value.unread; observedAt = value.observed_at;
        }
      }
      // Group flags do not identify the unread sender. Require an eligible retained
      // message when sender exclusions could otherwise count an ignored sender.
      if (flag === true && identity.endsWith('@g.us') && config.ownSenderIds?.length &&
          !pending.some(message => bare(message.conversationId) === identity)) flag = null;
      if (flag === null) unknown++;
      else { if (flag) count++; oldest = Math.min(oldest, observedAt); }
    }
    if (result.next_cursor === null) {
      // A just-received chat can race the paginated listing. Its freshly checked
      // pending unread identity is still useful evidence, counted only once.
      for (const identity of new Set(pending.map(message => bare(message.conversationId)))) {
        if (!seen.has(identity)) { count++; total++; seen.add(identity); }
      }
      // This is a lower bound from retained local state, not a whole-phone total.
      return { count, unknown, total, lookups, oldestStateAt: Number.isFinite(oldest) ? new Date(oldest * 1000).toISOString() : null };
    }
    if (!result.items.length || cursors.has(result.next_cursor)) throw fail('unread_page_invalid');
    cursor = result.next_cursor; cursors.add(cursor);
  }
  throw fail('unread_page_limit');
}

/** Direct MCP consumer; no provider credentials or secondary account connection. */
export default async function collect({ config, mcp, state, signal }) {
  if (!mcp || !jid(config.expectedAccountJid) || !config.expectedDatabaseId || !config.accountKey ||
      !Number.isSafeInteger(config.maximumPages) || config.maximumPages < 1 || config.maximumPages > 100 ||
      !Number.isSafeInteger(config.pageSize) || config.pageSize < 1 || config.pageSize > 25 ||
      !Number.isSafeInteger(config.maximumPending) || config.maximumPending < 1 || config.maximumPending > 100 ||
      !Number.isFinite(Date.parse(config.since))) throw fail('configuration_invalid');
  const key = 'whatsapp:' + config.accountKey;
  const own = new Set([config.expectedAccountJid, ...(config.ownSenderIds ?? [])].map(bare));
  const excluded = new Set([...(config.excludedConversationIds ?? []), '0@c.us'].map(bare));
  const allowed = (conversationId, senderId) => jid(conversationId) && jid(senderId) &&
    !own.has(bare(senderId)) && !own.has(bare(conversationId)) && !excluded.has(bare(conversationId)) &&
    (!config.allowedSenderIds?.length || config.allowedSenderIds.some(id => bare(id) === bare(senderId)));
  const previous = structuredClone(state ?? { cursor: 0, initialized: false, pending: {}, activity: [] });
  // A changed exclusion also applies to candidates retained by an earlier poll.
  for (const [id, message] of Object.entries(previous.pending))
    if (!allowed(message.conversationId, message.senderId)) delete previous.pending[id];
  const retained = new Set(Object.values(previous.pending).map(message => hash([message.conversationId, message.id])));
  previous.activity = previous.activity.filter(message => retained.has(hash([message.conversationId, message.id])));
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
        if (!allowed(message.chat_jid, message.sender_jid)) continue;
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
  let summary = null, summaryError = null;
  if (complete) {
    try {
      summary = await unreadSummary(config, mcp, signal, Object.values(next.pending));
      const guard = await mcp.call('whatsrust_events', { after: next.cursor, limit: 1, database_id: config.expectedDatabaseId });
      if (guard.account_jid !== config.expectedAccountJid || guard.database_id !== config.expectedDatabaseId || guard.gap === true) throw fail('source_identity_changed');
      const status = await mcp.call('whatsrust_status', {});
      if (status.ok !== true || status.connected !== true) throw fail('unread_source_unavailable');
    } catch (error) {
      signal.throwIfAborted(); summary = null;
      summaryError = /^[a-z][a-z0-9_]{0,63}$/.test(error?.code ?? '') ? error.code : 'unread_summary_unavailable';
      if (['source_identity_changed', 'unread_source_unavailable'].includes(summaryError)) {
        complete = false; issue = summaryError; next = structuredClone(previous);
      }
    }
  }
  const messages = Object.values(next.pending).sort(order);
  const observedAt = new Date().toISOString();
  accounts[key] = {
    application: 'whatsapp', displayName: config.displayName ?? 'WhatsApp', accountId: config.expectedAccountJid,
    ok: complete, status: complete ? 'ok' : 'trouble', state: summary ? 'partial' : complete ? 'healthy' : 'incomplete',
    unreadComplete: false, unreadCount: summary?.count ?? null,
    ...(summary ? { unreadCountKind: 'lower_bound', unreadCountUnit: 'conversations',
      unreadUnknownConversations: summary.unknown, unreadTotalConversations: summary.total,
      unreadCoverage: 'locally_synchronized', unreadOldestStateAt: summary.oldestStateAt } : {}),
    pendingUnreadComplete: complete, pendingUnreadCount: complete ? messages.length : null,
    pendingUnreadConversations: complete ? new Set(messages.map(m => m.conversationId)).size : null,
    observedAt, healthDetail: issue ?? summaryError ?? (summary ?
      `≥${summary.count} local unread chats; ${summary.unknown} chats have unknown read state. Full phone coverage is unverified.` : ''), messages: messages.slice(0, 50),
  };
  return {
    data: { schemaVersion: 1, accounts, activity: { [key]: next.activity ?? [] },
      observation: { complete, observedAt },
      source: { server: config.server ?? 'whatsapp', databaseId: config.expectedDatabaseId, cursor: next.cursor, journalComplete, baselineAt: next.baselineAt ?? null,
        coverage: 'live_events_since_baseline', unknownUnread, error: issue, unreadSummaryError: summaryError } },
    state: next,
  };
}
