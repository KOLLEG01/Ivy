import { createHash } from "node:crypto";

const fingerprint = (value) =>
  createHash("sha256").update(JSON.stringify(value)).digest("hex");
const text = (value, limit = 2000) =>
  typeof value === "string" ? value.slice(0, limit) : "";
const plain = (value) =>
  text(value, 20000)
    .replace(/<[^>]*>/g, " ")
    .replace(/&nbsp;/g, " ")
    .replace(/&amp;/g, "&")
    .replace(/\s+/g, " ")
    .trim();
const failure = (code) => Object.assign(new Error(code), { code });
const profileMatches = (actual, expected) =>
  actual?.id === expected.id &&
  actual?.email?.toLowerCase() === expected.email.toLowerCase();
const identity = (item) => ({
  id: item.id,
  conversationId: item.conversationId,
  revision: item.revision,
  occurredAt: item.occurredAt,
});
const activityOrder = (a, b) =>
  a.conversationId.localeCompare(b.conversationId) || a.id.localeCompare(b.id);
const base = (application, displayName, accountId) => ({
  application,
  displayName,
  accountId,
  ok: false,
  status: "trouble",
  state: "unknown",
  authState: "unknown",
  appReady: null,
  connectionState: "unknown",
  observedAt: null,
  unreadCount: null,
  unreadConversations: null,
  unreadComplete: false,
  healthDetail: "",
  messages: [],
  senders: [],
  retry: null,
});

async function outlook(config, mcp) {
  const profile = await mcp.call("microsoft_outlook_email.get_profile");
  if (!profileMatches(profile, config.profile))
    throw failure("authentication_required");
  let folderId = config.outlook.folderId;
  if (!folderId) {
    const folders = await mcp.call(
      "microsoft_outlook_email.list_mail_folders",
      { top: 200 },
    );
    const matches = (folders.value ?? []).filter(
      (folder) =>
        folder.path === config.outlook.folderName ||
        folder.well_known_name === config.outlook.folderName,
    );
    if (matches.length !== 1) throw failure("configuration_invalid");
    folderId = matches[0].id;
  }
  const messages = [];
  let skip = null,
    complete = false;
  for (let page = 0; page < (config.maximumPages ?? 50); page++) {
    const result = await mcp.call("microsoft_outlook_email.list_messages", {
      folder_id: folderId,
      filter: "isRead eq false",
      order_by: "receivedDateTime desc",
      top: 100,
      ...(skip === null ? {} : { skip }),
    });
    if (
      !Array.isArray(result.value) ||
      result.value.some(
        (mail) =>
          mail.isRead !== false ||
          typeof mail.id !== "string" ||
          !mail.id ||
          !Number.isFinite(
            Date.parse(
              mail.receivedDateTime ??
                mail.received_datetime ??
                mail.received_at,
            ),
          ),
      )
    )
      throw failure("provider_unavailable");
    messages.push(...result.value);
    if (result.has_more === false || (!result.has_more && !result.next_link)) {
      complete = true;
      break;
    }
    if (
      !Number.isSafeInteger(result.next_from_index) ||
      result.next_from_index <= (skip ?? 0)
    )
      throw failure("provider_unavailable");
    skip = result.next_from_index;
  }
  const unique = [...new Map(messages.map((mail) => [mail.id, mail])).values()];
  const items = unique.map((mail) => {
    const sender =
      mail.sender?.emailAddress ??
      mail.from?.emailAddress ??
      mail.sender ??
      mail.from ??
      {};
    const address = sender.address ?? sender.email ?? "";
    const content = mail.body?.content ?? mail.body ?? mail.body_preview ?? "";
    return {
      id: mail.id,
      conversationId: mail.conversation_id ?? mail.conversationId ?? mail.id,
      revision: fingerprint([
        mail.subject,
        content,
        mail.has_attachments ?? mail.hasAttachments,
      ]),
      occurredAt:
        mail.received_datetime ?? mail.receivedDateTime ?? mail.received_at,
      sender: sender.name || address,
      senderId: address,
      source: mail.subject || "Unread email",
      excerpt: plain(content),
      count: 1,
      url: mail.web_link ?? mail.webLink ?? mail.display_url ?? null,
      outgoing: address.toLowerCase() === config.profile.email.toLowerCase(),
      attachments:
        mail.has_attachments === true || mail.hasAttachments === true,
      reference: {
        server: "codex_apps",
        tool: "microsoft_outlook_email.fetch_message",
        messageId: mail.id,
      },
    };
  });
  return {
    folderId,
    complete,
    count: unique.length,
    conversations: new Set(items.map((item) => item.conversationId)).size,
    items: items.slice(0, config.maximumPreviews ?? 50),
    activity: items
      .filter((item) => !item.outgoing)
      .map(identity)
      .sort(activityOrder),
  };
}

async function gmail(config, mcp) {
  const profile = await mcp.call("gmail.get_profile");
  if (!profileMatches(profile, config.gmail.profile))
    throw failure("authentication_required");
  const labels = await mcp.call("gmail.list_labels", {
    label_names: ["INBOX"],
  });
  const inboxes = (labels.labels ?? []).filter((label) => label.id === "INBOX");
  const inbox = inboxes[0];
  if (
    inboxes.length !== 1 ||
    !Number.isSafeInteger(inbox.messagesUnread) ||
    inbox.messagesUnread < 0 ||
    !Number.isSafeInteger(inbox.threadsUnread) ||
    inbox.threadsUnread < 0 ||
    inbox.threadsUnread > inbox.messagesUnread
  )
    throw failure("provider_unavailable");
  const messages = new Map(),
    tokens = new Set();
  let token = null,
    complete = inbox.messagesUnread === 0;
  for (let page = 0; !complete && page < (config.maximumPages ?? 50); page++) {
    const result = await mcp.call("gmail.search_emails", {
      label_ids: ["INBOX", "UNREAD"],
      max_results: 100,
      ...(token === null ? {} : { next_page_token: token }),
    });
    if (
      !Array.isArray(result.emails) ||
      result.emails.some(
        (mail) =>
          typeof mail.id !== "string" ||
          !mail.id ||
          typeof mail.thread_id !== "string" ||
          !mail.thread_id ||
          !Array.isArray(mail.labels) ||
          !mail.labels.includes("INBOX") ||
          !mail.labels.includes("UNREAD") ||
          !Number.isFinite(Date.parse(mail.email_ts)),
      )
    )
      throw failure("provider_unavailable");
    for (const mail of result.emails) messages.set(mail.id, mail);
    if (result.next_page_token == null || result.next_page_token === "") {
      complete = true;
      break;
    }
    if (
      typeof result.next_page_token !== "string" ||
      tokens.has(result.next_page_token)
    )
      throw failure("provider_unavailable");
    token = result.next_page_token;
    tokens.add(token);
  }
  const items = [...messages.values()].map((mail) => {
    const sender = text(mail.from_, 500);
    const address =
      sender
        .match(/[^<>\s"]+@[^<>\s"]+/g)
        ?.at(-1)
        ?.replace(/[>,;]+$/, "") ?? "";
    return {
      id: mail.id,
      conversationId: mail.thread_id,
      revision: fingerprint([mail.subject, mail.snippet, mail.has_attachment]),
      occurredAt: mail.email_ts,
      sender,
      senderId: address,
      source: mail.subject || "Unread email",
      excerpt: plain(mail.snippet),
      count: 1,
      url: mail.display_url ?? null,
      outgoing:
        mail.labels.includes("SENT") ||
        address.toLowerCase() === config.gmail.profile.email.toLowerCase(),
      attachments: mail.has_attachment === true,
      reference: {
        server: "codex_apps",
        tool: "gmail.read_email",
        messageId: mail.id,
        threadId: mail.thread_id,
      },
    };
  });
  complete =
    complete &&
    messages.size === inbox.messagesUnread &&
    new Set(items.map((item) => item.conversationId)).size ===
      inbox.threadsUnread;
  return {
    complete,
    count: inbox.messagesUnread,
    conversations: inbox.threadsUnread,
    items: items.slice(0, config.maximumPreviews ?? 50),
    activity: items
      .filter((item) => !item.outgoing)
      .map(identity)
      .sort(activityOrder),
  };
}

async function teams(config, mcp) {
  const profile = await mcp.call("microsoft_teams.get_profile");
  if (!profileMatches(profile, config.profile))
    throw failure("authentication_required");
  const result = await mcp.call("microsoft_teams.list_chats", {
    top: 100,
    unread_only: true,
  });
  if (!Array.isArray(result.chats)) throw failure("provider_unavailable");
  let complete = result.chats.length < 100,
    conversations = 0;
  const items = [];
  if (result.chats.some((chat) => typeof chat.id !== "string" || !chat.id))
    throw failure("provider_unavailable");
  for (const chat of new Map(
    result.chats.map((chat) => [chat.id, chat]),
  ).values()) {
    if (
      (config.teams.excludedChatIds ?? []).includes(chat.id) ||
      chat.is_hidden === true
    )
      continue;
    if (
      chat.is_unread !== true ||
      !Number.isFinite(Date.parse(chat.last_message_read_at))
    ) {
      complete = false;
      continue;
    }
    const page = await mcp.call("microsoft_teams.list_chat_messages", {
      chat_id: chat.id,
      top: 100,
      sent_after: chat.last_message_read_at,
    });
    if (!Array.isArray(page.messages)) throw failure("provider_unavailable");
    if (page.messages.length >= 100) complete = false;
    const direct = ["oneOnOne", "one_on_one", "direct"].includes(
      chat.chat_type,
    );
    let memberCount = null;
    if (!direct && config.teams.maximumGroupMembersExclusive) {
      const members = await mcp.call("microsoft_teams.get_chat_members", {
        chat_id: chat.id,
      });
      const list = members.members ?? members.value;
      if (!Array.isArray(list)) {
        complete = false;
        continue;
      }
      memberCount = list.length;
    }
    let counted = false;
    for (const message of new Map(
      page.messages.map((message) => [message.message_id, message]),
    ).values()) {
      if (
        typeof message.message_id !== "string" ||
        !message.message_id ||
        !Number.isFinite(Date.parse(message.created_at))
      ) {
        complete = false;
        continue;
      }
      if (
        message.author_user_id === config.profile.id ||
        message.deleted_at ||
        message.message_type !== "message" ||
        Date.parse(message.created_at) <= Date.parse(chat.last_message_read_at)
      )
        continue;
      const mentioned = (message.mentions ?? []).some(
        (mention) => mention.mentioned_user_id === config.profile.id,
      );
      if (!direct && config.teams.groupPolicy === "mentions" && !mentioned)
        continue;
      if (
        !direct &&
        memberCount >= config.teams.maximumGroupMembersExclusive &&
        !mentioned
      )
        continue;
      items.push({
        id: message.message_id,
        conversationId: chat.id,
        revision: fingerprint([
          message.content,
          message.attachments,
          message.mentions,
        ]),
        occurredAt: message.created_at,
        sender: message.author_name ?? "",
        senderId: message.author_user_id ?? "",
        source: chat.topic || message.author_name || "Teams",
        excerpt: plain(message.content),
        count: 1,
        url: message.web_link ?? message.display_url ?? chat.webUrl ?? null,
        outgoing: false,
        attachments: message.has_attachments === true,
        reference: {
          server: "codex_apps",
          tool: "microsoft_teams.list_chat_messages",
          chatId: chat.id,
          messageId: message.message_id,
        },
      });
      counted = true;
    }
    if (counted) conversations++;
  }
  return {
    complete,
    count: items.length,
    conversations,
    items: items.slice(0, config.maximumPreviews ?? 50),
    activity: items.map(identity).sort(activityOrder),
  };
}

export default async function collect({ config, mcp, state, signal }) {
  if (
    !mcp ||
    !config.profile?.id ||
    !config.profile?.email ||
    !config.accountKey ||
    !config.outlook ||
    !config.teams ||
    !["mentions", "small-groups-or-mentions"].includes(
      config.teams.groupPolicy,
    ) ||
    !Number.isSafeInteger(config.teams.maximumGroupMembersExclusive) ||
    config.teams.maximumGroupMembersExclusive < 2 ||
    (config.gmail &&
      (typeof config.gmail.accountKey !== "string" ||
        !config.gmail.accountKey ||
        typeof config.gmail.profile?.id !== "string" ||
        !config.gmail.profile.id ||
        typeof config.gmail.profile?.email !== "string" ||
        !config.gmail.profile.email))
  )
    throw failure("configuration_invalid");
  const previous = state ?? { accounts: {}, activity: {} },
    accounts = {},
    activity = {};
  let inboxFolderId = previous.inboxFolderId ?? null;
  const sources = [
    ["outlook", outlook, config.accountKey, config.profile],
    ["teams", teams, config.accountKey, config.profile],
  ];
  if (config.gmail)
    sources.push([
      "gmail",
      gmail,
      config.gmail.accountKey,
      config.gmail.profile,
    ]);
  for (const [application, read, accountKey, profile] of sources) {
    signal.throwIfAborted();
    const key = application + ":" + accountKey;
    const account = base(
      application,
      config[application].displayName ?? config.displayName,
      profile.id,
    );
    try {
      const result = await read(
        {
          ...config,
          outlook: {
            ...config.outlook,
            folderId: config.outlook.folderId ?? inboxFolderId,
          },
        },
        mcp,
      );
      const observedAt = new Date().toISOString();
      accounts[key] = {
        ...account,
        ok: result.complete,
        status: result.complete ? "ok" : "trouble",
        state: result.complete ? "healthy" : "incomplete",
        authState: "authenticated",
        appReady: true,
        connectionState: "online",
        observedAt,
        unreadCount: result.complete
          ? result.count
          : (previous.accounts?.[key]?.unreadCount ?? null),
        unreadConversations: result.complete
          ? result.conversations
          : (previous.accounts?.[key]?.unreadConversations ?? null),
        unreadComplete: result.complete,
        healthDetail: result.complete
          ? ""
          : "The connector result is incomplete.",
        messages: result.items,
        senders: [
          ...new Set(result.items.map((item) => item.sender).filter(Boolean)),
        ],
      };
      activity[key] = result.complete
        ? result.activity
        : (previous.activity?.[key] ?? []);
      if (application === "outlook") inboxFolderId = result.folderId;
    } catch (error) {
      signal.throwIfAborted();
      const code = [
        "authentication_required",
        "configuration_invalid",
      ].includes(error?.code)
        ? error.code
        : "provider_unavailable";
      accounts[key] = {
        ...(previous.accounts?.[key] ?? account),
        ok: false,
        status: "trouble",
        state: "unavailable",
        authState:
          code === "authentication_required"
            ? "unknown"
            : (previous.accounts?.[key]?.authState ?? "unknown"),
        unreadComplete: false,
        healthDetail: code,
      };
      activity[key] = previous.activity?.[key] ?? [];
    }
  }
  // Activity has no poll timestamps: object watchers can select /data/activity.
  const observedAt = new Date().toISOString(),
    complete = Object.values(accounts).every(
      (account) => account.unreadComplete,
    );
  return {
    data: {
      schemaVersion: 1,
      generatedAt: observedAt,
      accounts,
      activity,
      observation: { complete, observedAt },
    },
    state: { accounts, activity, inboxFolderId },
  };
}
