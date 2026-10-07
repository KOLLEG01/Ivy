import test from "node:test";
import assert from "node:assert/strict";
import collect from "../docs/examples/data-collector/m365/m365.mjs";

const profile = { id: "self", email: "me@example.test" };
const config = {
  profile,
  accountKey: "work",
  displayName: "Work",
  outlook: { folderName: "Inbox" },
  teams: {
    groupPolicy: "mentions",
    maximumGroupMembersExclusive: 10,
    excludedChatIds: [],
  },
  maximumPages: 3,
  maximumPreviews: 1,
};
const mail = (id, sender = "other@example.test") => ({
  id,
  isRead: false,
  subject: "Subject",
  body: { content: "<p>Content</p>" },
  sender: { emailAddress: { address: sender, name: "Sender" } },
  receivedDateTime: "2026-10-07T08:00:00.000Z",
});
const mcp = (respond) => ({
  async call(tool, args) {
    if (tool.endsWith(".get_profile")) return profile;
    if (tool === "microsoft_outlook_email.list_mail_folders")
      return { value: [{ id: "inbox", path: "Inbox" }] };
    if (tool === "microsoft_teams.list_chats") return { chats: [] };
    return await respond(tool, args);
  },
});
const run = (connector, state = null, overrides = {}) =>
  collect({
    config: { ...config, ...overrides },
    mcp: connector,
    state,
    signal: new AbortController().signal,
  });

test("M365 collector paginates only the selected Inbox and publishes stable content identities beyond preview limits", async () => {
  const connector = mcp((tool, args) => {
    assert.equal(tool, "microsoft_outlook_email.list_messages");
    assert.equal(args.folder_id, "inbox");
    assert.equal(args.filter, "isRead eq false");
    return args.skip === 100
      ? { value: [mail("own", profile.email)], has_more: false }
      : {
          value: [mail("first"), mail("second")],
          has_more: true,
          next_from_index: 100,
        };
  });
  const result = await run(connector);
  assert.equal(result.data.accounts["outlook:work"].unreadCount, 3);
  assert.equal(result.data.accounts["outlook:work"].messages.length, 1);
  assert.equal(result.data.activity["outlook:work"].length, 2);
  assert.equal(result.data.observation.complete, true);
  const repeated = await run(connector, result.state);
  assert.deepEqual(repeated.data.activity, result.data.activity);
});

test("M365 account mismatches and incomplete pagination retain prior evidence without asserting a healthy zero", async () => {
  const good = await run(
    mcp(() => ({ value: [mail("one")], has_more: false })),
  );
  const partial = await run(
    mcp(() => ({ value: [], has_more: true, next_from_index: null })),
    good.state,
  );
  assert.equal(partial.data.accounts["outlook:work"].unreadCount, 1);
  assert.equal(partial.data.accounts["outlook:work"].ok, false);
  assert.equal(partial.data.observation.complete, false);
  assert.deepEqual(
    partial.data.activity["outlook:work"],
    good.data.activity["outlook:work"],
  );
  let reads = 0;
  const wrong = await run(
    {
      async call() {
        reads++;
        return { id: "different", email: profile.email };
      },
    },
    good.state,
  );
  assert.equal(
    reads,
    2,
    "Each connector identity is checked before any message read.",
  );
  assert.equal(
    wrong.data.accounts["outlook:work"].healthDetail,
    "authentication_required",
  );
  assert.equal(wrong.data.accounts["outlook:work"].unreadCount, 1);
  assert.deepEqual(wrong.data.activity, good.data.activity);
});

test("Teams eligibility counts incoming messages and mentions, and treats missing read watermarks as incomplete", async () => {
  const direct = {
    id: "direct",
    chat_type: "oneOnOne",
    is_unread: true,
    last_message_read_at: "2026-10-07T07:00:00.000Z",
  };
  const message = (id, author = "other", mentions = []) => ({
    message_id: id,
    author_user_id: author,
    message_type: "message",
    content: "Message",
    created_at: "2026-10-07T08:00:00.000Z",
    mentions,
  });
  const connector = {
    async call(tool, args) {
      if (tool.endsWith(".get_profile")) return profile;
      if (tool.endsWith(".list_mail_folders"))
        return { value: [{ id: "inbox", path: "Inbox" }] };
      if (tool.endsWith(".list_messages"))
        return { value: [], has_more: false };
      if (tool.endsWith(".list_chats"))
        return {
          chats: [direct, { ...direct, id: "group", chat_type: "group" }],
        };
      if (tool.endsWith(".get_chat_members"))
        return { members: Array.from({ length: 20 }, (_, id) => ({ id })) };
      assert.equal(tool, "microsoft_teams.list_chat_messages");
      assert.equal(args.sent_after, direct.last_message_read_at);
      return {
        messages:
          args.chat_id === "direct"
            ? [message("own", "self"), message("incoming")]
            : [
                message("unmentioned"),
                message("mention", "other", [{ mentioned_user_id: "self" }]),
              ],
      };
    },
  };
  const result = await run(connector);
  assert.equal(result.data.accounts["teams:work"].unreadCount, 2);
  assert.equal(result.data.activity["teams:work"].length, 2);
  direct.last_message_read_at = null;
  const incomplete = await run(connector, result.state);
  assert.equal(incomplete.data.accounts["teams:work"].unreadCount, 2);
  assert.equal(incomplete.data.accounts["teams:work"].unreadComplete, false);
});

test("Teams small-group policy uses an exclusive member limit and deduplicates native message identities", async () => {
  const chat = (id) => ({
    id,
    chat_type: "group",
    is_unread: true,
    last_message_read_at: "2026-10-07T07:00:00.000Z",
  });
  const message = (id, mentions = []) => ({
    message_id: id,
    author_user_id: "other",
    message_type: "message",
    content: "Message",
    created_at: "2026-10-07T08:00:00.000Z",
    mentions,
  });
  let malformed = false;
  const connector = {
    async call(tool, args) {
      if (tool.endsWith(".get_profile")) return profile;
      if (tool.endsWith(".list_mail_folders"))
        return { value: [{ id: "inbox", path: "Inbox" }] };
      if (tool.endsWith(".list_messages"))
        return { value: [], has_more: false };
      if (tool.endsWith(".list_chats"))
        return {
          chats: [
            chat("small"),
            chat("small"),
            chat("large"),
            chat("excluded"),
          ],
        };
      assert.notEqual(args.chat_id, "excluded");
      if (tool.endsWith(".get_chat_members"))
        return {
          members: Array.from(
            { length: args.chat_id === "small" ? 4 : 5 },
            (_, id) => ({ id }),
          ),
        };
      assert.equal(tool, "microsoft_teams.list_chat_messages");
      return {
        messages:
          args.chat_id === "small"
            ? [
                message("incoming"),
                message("incoming"),
                { ...message("old"), created_at: "2026-10-07T07:00:00.000Z" },
              ]
            : [
                message("unmentioned"),
                message("mention", [{ mentioned_user_id: "self" }]),
                ...(malformed
                  ? [{ ...message("invalid"), created_at: "invalid" }]
                  : []),
              ],
      };
    },
  };
  const teams = {
    groupPolicy: "small-groups-or-mentions",
    maximumGroupMembersExclusive: 5,
    excludedChatIds: ["excluded"],
  };
  const result = await run(connector, null, { teams });
  assert.equal(result.data.accounts["teams:work"].unreadCount, 2);
  assert.equal(result.data.observation.complete, true);
  assert.deepEqual(
    result.data.activity["teams:work"].map((item) => item.id),
    ["mention", "incoming"],
  );
  malformed = true;
  const incomplete = await run(connector, result.state, { teams });
  assert.equal(incomplete.data.observation.complete, false);
  assert.deepEqual(incomplete.data.activity, result.data.activity);
});

const gmailConfig = {
  accountKey: "personal",
  displayName: "Gmail",
  profile: { id: "google-self", email: "google@example.test" },
};
const gmailMail = (id, thread = id, labels = ["INBOX", "UNREAD"]) => ({
  id,
  thread_id: thread,
  labels,
  from_: "Sender sender@example.test",
  subject: "Subject",
  snippet: "Content",
  email_ts: "2026-10-07T08:00:00+00:00",
  has_attachment: false,
  display_url: "https://mail.example.test/" + id,
});
const gmailMcp = (read, counts = { messagesUnread: 1, threadsUnread: 1 }) => {
  const microsoft = mcp(() => ({ value: [], has_more: false }));
  return {
    async call(tool, args) {
      if (tool === "gmail.get_profile") return gmailConfig.profile;
      if (tool === "gmail.list_labels") {
        assert.deepEqual(args, { label_names: ["INBOX"] });
        return { labels: [{ id: "INBOX", ...counts }] };
      }
      if (tool === "gmail.search_emails") {
        assert.deepEqual(args.label_ids, ["INBOX", "UNREAD"]);
        assert.equal(args.max_results, 100);
        return read(args);
      }
      assert.ok(
        !tool.startsWith("gmail."),
        "Only granted Gmail reads are used.",
      );
      return microsoft.call(tool, args);
    },
  };
};

test("Gmail uses its separate profile, deduplicates paginated inbox messages and preserves stable incoming activity beyond previews", async () => {
  const connector = gmailMcp(
    (args) =>
      args.next_page_token
        ? {
            emails: [gmailMail("own", "own", ["INBOX", "UNREAD", "SENT"])],
            next_page_token: null,
          }
        : {
            emails: [
              gmailMail("one", "thread"),
              gmailMail("one", "thread"),
              gmailMail("two", "thread"),
            ],
            next_page_token: "next",
          },
    { messagesUnread: 3, threadsUnread: 2 },
  );
  const result = await run(connector, null, { gmail: gmailConfig });
  const account = result.data.accounts["gmail:personal"];
  assert.equal(account.accountId, gmailConfig.profile.id);
  assert.equal(account.unreadCount, 3);
  assert.equal(account.unreadConversations, 2);
  assert.equal(account.messages.length, 1);
  assert.equal(account.messages[0].reference.tool, "gmail.read_email");
  assert.equal(account.messages[0].reference.messageId, "one");
  assert.deepEqual(
    result.data.activity["gmail:personal"].map((item) => item.id),
    ["one", "two"],
  );
  assert.equal(result.data.observation.complete, true);
  const repeated = await run(connector, result.state, { gmail: gmailConfig });
  assert.deepEqual(repeated.data.activity, result.data.activity);
});

test("Gmail caps, repeated cursors, changed label totals and invalid unread evidence retain the previous account without disrupting Microsoft counts", async () => {
  const good = await run(
    gmailMcp(() => ({ emails: [gmailMail("one")], next_page_token: null })),
    null,
    { gmail: gmailConfig },
  );
  const cases = [
    [
      gmailMcp(() => ({ emails: [], next_page_token: "more" })),
      { maximumPages: 1 },
    ],
    [
      gmailMcp(() => ({
        emails: [gmailMail("one")],
        next_page_token: "repeat",
      })),
      {},
    ],
    [gmailMcp(() => ({ emails: [], next_page_token: null })), {}],
    [
      gmailMcp(() => ({
        emails: [gmailMail("read", "read", ["INBOX"])],
        next_page_token: null,
      })),
      {},
    ],
    [
      gmailMcp(() => ({
        emails: [{ ...gmailMail("one"), email_ts: "invalid" }],
        next_page_token: null,
      })),
      {},
    ],
  ];
  for (const [connector, overrides] of cases) {
    const partial = await run(connector, good.state, {
      gmail: gmailConfig,
      ...overrides,
    });
    assert.equal(partial.data.accounts["gmail:personal"].unreadCount, 1);
    assert.equal(partial.data.accounts["gmail:personal"].ok, false);
    assert.equal(partial.data.accounts["gmail:personal"].unreadComplete, false);
    assert.deepEqual(
      partial.data.activity["gmail:personal"],
      good.data.activity["gmail:personal"],
    );
    assert.equal(partial.data.accounts["outlook:work"].ok, true);
    assert.equal(partial.data.accounts["teams:work"].ok, true);
    assert.equal(partial.data.observation.complete, false);
  }
});

test("Gmail rejects a mismatched Google identity before mailbox access", async () => {
  const goodConnector = gmailMcp(() => ({
    emails: [gmailMail("one")],
    next_page_token: null,
  }));
  const good = await run(goodConnector, null, { gmail: gmailConfig });
  const gmailCalls = [];
  const wrong = {
    async call(tool, args) {
      if (!tool.startsWith("gmail.")) return goodConnector.call(tool, args);
      gmailCalls.push(tool);
      if (tool === "gmail.get_profile")
        return { ...gmailConfig.profile, id: "wrong-google-user" };
      throw new Error(
        "Mailbox access before matching the expected Gmail account",
      );
    },
  };
  const result = await run(wrong, good.state, { gmail: gmailConfig });
  assert.deepEqual(gmailCalls, ["gmail.get_profile"]);
  assert.equal(
    result.data.accounts["gmail:personal"].healthDetail,
    "authentication_required",
  );
  assert.equal(result.data.accounts["gmail:personal"].unreadCount, 1);
  assert.deepEqual(
    result.data.activity["gmail:personal"],
    good.data.activity["gmail:personal"],
  );
});

test("Gmail accepts an authoritative empty inbox and removes previously unread activity without fetching messages", async () => {
  const good = await run(
    gmailMcp(() => ({ emails: [gmailMail("one")], next_page_token: null })),
    null,
    { gmail: gmailConfig },
  );
  const empty = gmailMcp(
    () => {
      throw new Error(
        "No message fetch is needed for verified zero unread mail",
      );
    },
    { messagesUnread: 0, threadsUnread: 0 },
  );
  const result = await run(empty, good.state, { gmail: gmailConfig });
  assert.equal(result.data.accounts["gmail:personal"].unreadCount, 0);
  assert.deepEqual(result.data.activity["gmail:personal"], []);
  assert.equal(result.data.observation.complete, true);
});
