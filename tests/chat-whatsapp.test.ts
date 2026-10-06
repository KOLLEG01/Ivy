import test from "node:test";
import type { Worker } from "node:worker_threads";
import childProcess from "node:child_process";
import { EventEmitter } from "node:events";
import { createCipheriv, createHash, createHmac, hkdfSync } from "node:crypto";
import { proto } from "baileys";
import { syncBuiltinESMExports } from "node:module";
import { desktopCommand } from "../services/chat-bridge/src/whatsapp/desktop.js";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { WASocket, WAMessage } from "baileys";
import type { ChatBridge } from "../services/chat-bridge/src/bridge.js";
import { WhatsAppJournal } from "../services/chat-bridge/src/whatsapp/journal.js";
import { WhatsAppSocket } from "../services/chat-bridge/src/whatsapp/socket.js";
import {
  WhatsAppCommands,
  parseCommand,
} from "../services/chat-bridge/src/whatsapp/commands.js";
import {
  WhatsAppListener,
  whatsappText,
  delegationSource,
  terminalText,
} from "../services/chat-bridge/src/whatsapp/listener.js";
import {
  commandText,
  mediaPayload,
} from "../services/chat-bridge/src/whatsapp/media.js";
import {
  encode,
  WhatsAppAuthStore,
} from "../services/chat-bridge/src/whatsapp/auth.js";
import { IvyError } from "../packages/contracts/src/errors.js";
import { parseOperationId } from "../packages/contracts/src/operation-id.js";
import { WhatsAppRuntime } from "../services/chat-bridge/src/whatsapp/runtime.js";
import type { Agent, Chat } from "../packages/contracts/src/generated.js";
import type { WhatsAppNative } from "../services/chat-bridge/src/whatsapp/native.js";
import type { WhatsAppConfig } from "../services/chat-bridge/src/whatsapp/config.js";
import {
  decodedAudioDurationSeconds,
  encodeMonoPcm16Wav,
} from "../services/chat-bridge/src/whatsapp/audio-processing.js";
import { translator } from "../services/chat-bridge/src/whatsapp/messages.js";
import { ChatStore, mutation } from '../services/chat-bridge/src/store.js';
import type { RpcClient } from '../packages/sdk/src/client.js';
import { attributedNoticeReply } from '../services/chat-bridge/src/notice-input.js';

test('service notices deliver the canonical reply once, retain attribution after restart and confirm only sent text', async t => {
  let store: ChatStore;
  t.after(() => store?.close());
  const { journal, root } = fixture(t);
  const client = { request: async () => ({ runtimeEpoch: 'notice-test' }) } as unknown as RpcClient;
  store = new ChatStore(client, null, root);
  const clientId = mutation('notice-input', 'native-user-message');
  store.rememberNoticeInput('notice-input');
  store.close(); store = new ChatStore(client, null, root);
  assert.equal(store.isNoticeInput(clientId), true);
  assert.equal(store.isNoticeInput('wa-user-input'), false);
  const binding = { pin: { objectId: 'binding', revision: 1 }, value: { primary: { nativeId: 'main' } } };
  const original = 'Diane sent a property link: https://example.test/property';
  const items = [{ type: 'userMessage', clientId, content: [] }, { type: 'agentMessage', id: 'progress', phase: 'commentary', text: 'Preparing notice' },
    { type: 'agentMessage', id: 'final', phase: 'final_answer', text: original }];
  let acknowledgements = 0, confirmed = false;
  const bridge = { main: { store, operations: { find: async () => ({}) }, admission: { definitionHash: 'partition', expected: () => ({}) } }, outbox: {
    acknowledge: async () => { acknowledgements++; return { phase: 'succeeded' }; },
    history: async () => ({ entries: [], throughSequence: 0 }),
    pendingNotices: async () => ({ replies: [{ pin: { objectId: 'reply' }, value: { text: attributedNoticeReply('secretary', original) } }], nextCursor: null }),
    noticeParts: async (reply: unknown) => [reply],
    reply: async () => ({ value: { state: confirmed ? 'confirmed' : 'pending' } }),
    confirmNotice: async () => { confirmed = true; },
  } } as unknown as ChatBridge;
  const native = { bound: async () => ({ binding }), turnPage: async () => ({ data: [{ id: 'turn', status: 'completed', itemsView: 'full', items }], nextCursor: null }) } as unknown as WhatsAppNative;
  const listener = new WhatsAppListener(bridge, journal, native, config);
  journal.set('listener-new:binding', true);
  await listener.notification({ method: 'item/completed', params: { threadId: 'main', turnId: 'turn', item: items[2] } } as unknown as Agent.Notification, true);
  assert.equal(journal.outgoing().length, 0, 'Out-of-order final output must wait for its origin.');
  await listener.tick(true);
  assert.equal(journal.outgoing().length, 0, 'Raw notice and commentary stay private.');
  const text = attributedNoticeReply('secretary', original);
  journal.set('offer:partition:main-wa', { request: {}, offer: { object: { objectId: 'offer' }, data: { throughSequence: 3 } }, replies: [{
    object: { objectId: 'reply' }, data: { text, origin: { kind: 'native', inputId: 'notice-input', binding: binding.pin } },
  }] });
  await listener.notices();
  await listener.outbox(); await listener.outbox();
  assert.deepEqual(journal.outgoing().map(row => row.text), [text]);
  assert.equal(acknowledgements, 0);
  for (const row of journal.outgoing()) journal.mark(row.id, 'sent');
  await new WhatsAppListener(bridge, journal, native, config).notices();
  await new WhatsAppListener(bridge, journal, native, config).outbox();
  assert.equal(acknowledgements, 1);
  assert.equal(journal.get('reply-cursor:partition:main-wa'), 3);
  await listener.notices();
  await new WhatsAppListener(bridge, journal, native, config).notices();
  assert.equal(journal.outgoing().length, 0, 'The same reply must reuse its sent WhatsApp receipt after restart.');
  assert.equal(acknowledgements, 1);
});

test("voice conversion retains duration and emits bounded mono PCM16 at 16 kHz", () => {
  const audio = {
    sampleRate: 48000,
    channelData: [
      new Float32Array(48000).fill(1),
      new Float32Array(48000).fill(-1),
    ],
  };
  assert.equal(decodedAudioDurationSeconds(audio), 1);
  const wav = encodeMonoPcm16Wav(audio);
  assert.equal(wav.length, 32044);
  assert.equal(wav.readUInt32LE(24), 16000);
  assert.equal(wav.readUInt16LE(22), 1);
  assert.equal(wav.readInt16LE(44), 0);
  assert.throws(() => encodeMonoPcm16Wav({ sampleRate: 0, channelData: [] }));
});
const config: WhatsAppConfig = {
  accountId: "bot",
  botPhoneNumber: "+15555550100",
  since: "2026-09-10T00:00:00.000Z",
  senders: [
    { phoneNumber: "+15555550200", principalId: "user", channelId: "main-wa" },
  ],
};
function fixture(t: test.TestContext) {
  const root = mkdtempSync(join(tmpdir(), "ivy-wa-")),
    journal = new WhatsAppJournal(join(root, "journal.sqlite")),
    auth = new WhatsAppAuthStore(join(root, "auth.sqlite"));
  t.after(async () => {
    try {
      await journal.close();
    } catch {}
    try {
      auth.close();
    } catch {}
    rmSync(root, { recursive: true, force: true });
  });
  return { root, journal, auth };
}
const input = {
  id: "input",
  sender: "user",
  jid: "15555550200@s.whatsapp.net",
  message: "original",
  received: 1,
};
test("WhatsApp journal preserves input, exact request, Unicode chunks and uncertain delivery across restart", async (t) => {
  const { journal, root } = fixture(t);
  journal.accept(input);
  journal.accept(input);
  assert.equal(journal.pending().length, 1);
  assert.throws(() => journal.accept({ ...input, message: "changed" }));
  journal.saveRequest(input.id, { binding: "original" });
  journal.saveRequest(input.id, { binding: "replacement" });
  assert.equal(journal.pending()[0]!.request, '{"binding":"original"}');
  const text = "😀".repeat(7001),
    ids = journal.enqueue("reply", input.jid, text);
  assert.equal(ids.length, 3);
  assert.equal(
    journal
      .outgoing()
      .map((r) => r.text)
      .join(""),
    text,
  );
  journal.mark(ids[0]!, "sending");
  await journal.close();
  const reopened = new WhatsAppJournal(join(root, "journal.sqlite"));
  assert.equal(reopened.outgoing()[0]!.state, "sending");
  assert.equal(reopened.sent(ids), false);
  await reopened.close();
});
test("WhatsApp unknown send is never repeated; independent recipients still progress", async (t) => {
  const { journal, auth, root } = fixture(t),
    transport = new WhatsAppSocket(config, journal, auth, root, () => {});
  let sends = 0;
  transport.connected = true;
  transport.socket = {
    sendMessage: async () => {
      sends++;
      throw Error("lost reply");
    },
  } as unknown as WASocket;
  journal.enqueue("reply", input.jid, "answer");
  await transport.flush();
  await transport.flush();
  assert.equal(sends, 1);
  assert.equal(transport.errorCode, "whatsapp_delivery_unknown");
  transport.socket = {
    sendMessage: async () => {
      sends++;
      return {};
    },
  } as unknown as WASocket;
  journal.enqueue("other", "15555550300@s.whatsapp.net", "independent");
  await transport.flush();
  assert.equal(sends, 2);
});

test("transport shutdown rejects when its writer exits before the last durable batch", async (t) => {
  const { journal } = fixture(t);
  const writer = (journal as unknown as { writer: Worker }).writer;
  t.mock.method(writer, "postMessage", () => {});
  journal.set("uncommitted", true);
  const closing = journal.close();
  writer.emit("exit", 1);
  let fallback = false;
  const timer = setTimeout(() => {
    fallback = true;
    writer.emit("error", Error("Test cleanup"));
  }, 1000);
  try {
    await assert.rejects(closing, { code: "storage_unavailable" });
    assert.equal(
      fallback,
      false,
      "Writer exit must reject pending flushes even during close.",
    );
  } finally {
    clearTimeout(timer);
  }
});

test("a new Main catches up every page before publishing its first listener watermark", async (t) => {
  const journal = new WhatsAppJournal(":memory:");
  t.after(() => journal.close());
  const turns = Array.from({ length: 70 }, (_, i) => ({
    id: `turn-${i}`,
    status: "completed",
    itemsView: "full",
    items: [{ id: `item-${i}`, type: "agentMessage", text: `Answer ${i}` }],
  }));
  const native = {
    bound: async () => ({
      binding: {
        pin: { objectId: "binding" },
        value: { primary: { nativeId: "main" } },
      },
    }),
    turnPage: async (
      _threadId: string,
      cursor: string | null,
    ) => {
      const offset = Number(cursor ?? 0),
        page = [...turns].reverse().slice(offset, offset + 1);
      return {
        data: page,
        nextCursor:
          offset + page.length < turns.length
            ? String(offset + page.length)
            : null,
      };
    },
  } as unknown as WhatsAppNative;
  journal.set("listener-new:binding", true);
  const listener = new WhatsAppListener(
    {} as ChatBridge,
    journal,
    native,
    config,
  );
  await listener.tick(false);
  assert.equal(listener.needsRecovery, true);
  assert.equal(
    journal.get<{ lastTurn?: string }>("listener:binding")!.lastTurn,
    undefined,
  );
  await listener.tick(false);
  assert.equal(listener.needsRecovery, false);
  assert.deepEqual(
    journal.db
      .prepare("SELECT text FROM outgoing ORDER BY sequence")
      .all()
      .map((row) => row["text"]),
    turns.map((turn) => turn.items[0]!.text),
  );
  await listener.tick(false);
  assert.equal(
    journal.db.prepare("SELECT count(*) AS n FROM outgoing").get()!["n"],
    70,
  );
});

test(
  "desktop commands persist their intent before spawning and reuse the saved result",
  { skip: process.platform !== "win32" },
  async (t) => {
    const journal = new WhatsAppJournal(":memory:");
    t.after(() => journal.close());
    let release!: () => void,
      calls = 0;
    const fence = new Promise<void>((resolve) => {
      release = resolve;
    });
    const flush = t.mock.method(journal, "flush", () => fence);
    const spawn = t.mock.method(childProcess, "spawn", ((
      _program: string,
      args: string[],
    ) => {
      calls++;
      const script = Buffer.from(args.at(-1)!, "base64").toString("utf16le");
      assert.match(script, /shell:AppsFolder\\/);
      const child = new EventEmitter();
      queueMicrotask(() => child.emit("exit", 0));
      return child;
    }) as typeof childProcess.spawn);
    syncBuiltinESMExports();
    t.after(() => {
      spawn.mock.restore();
      syncBuiltinESMExports();
    });
    const running = desktopCommand("ensure", "original", journal);
    try {
      assert.equal(flush.mock.callCount(), 1);
      assert.equal(calls, 0);
    } finally {
      release();
    }
    const result = await running;
    assert.equal(await desktopCommand("ensure", "original", journal), result);
    assert.equal(calls, 1);
  },
);

test("retention advances past protected rows and preserves partial turn delivery", async (t) => {
  const journal = new WhatsAppJournal(":memory:");
  t.after(() => journal.close());
  const old = Date.now() - 8 * 86400_000;
  for (let i = 0; i < 256; i++) {
    const ids = journal.enqueue(`protected-${i}`, input.jid, "Keep");
    journal.mark(ids[0]!, "sent");
    journal.set("out-created:" + ids[0], old);
    journal.set(`message:binding:current:item-${i}:sender`, { ids, at: old });
  }
  journal.set("listener:binding", { lastTurn: "current" });
  const expired = journal.enqueue("expired", input.jid, "Remove");
  journal.mark(expired[0]!, "sent");
  journal.set("out-created:" + expired[0], old);
  journal.set("message:binding:older:item:sender", { ids: expired, at: old });
  const partial = journal.enqueue("partial", input.jid, "x".repeat(3501));
  journal.mark(partial[0]!, "sent");
  journal.set("out-created:" + partial[0], old);
  journal.set("turn-delivered:binding:partial", partial);
  journal.prune();
  assert.equal(journal.get("message:binding:older:item:sender"), null);
  assert.equal(
    journal.db.prepare("SELECT 1 FROM outgoing WHERE id=?").get(expired[0]!),
    undefined,
  );
  assert.equal(journal.sent([partial[0]!]), true);
  journal.mark(partial[1]!, "sent");
  journal.prune();
  assert.equal(journal.get("turn-delivered:binding:partial"), true);
});
test("WhatsApp sender admission rejects groups, own messages, unknown IDs and contradictory LID mappings before journal writes", async (t) => {
  const { journal, auth, root } = fixture(t),
    transport = new WhatsAppSocket(config, journal, auth, root, () => {});
  const socket = {
    signalRepository: {
      lidMapping: { getPNForLID: async () => "15555550200@s.whatsapp.net" },
    },
  } as unknown as WASocket;
  const message = (jid: string, extra = {}) =>
    ({ key: { id: "m", remoteJid: jid, ...extra } }) as WAMessage;
  assert.ok(await transport.admitted(message(input.jid), socket));
  assert.ok(await transport.admitted(message("123@lid"), socket));
  for (const msg of [
    message("123@g.us"),
    message(input.jid, { fromMe: true }),
    message("15555550999@s.whatsapp.net"),
    message("123@lid", { remoteJidAlt: "15555550999@s.whatsapp.net" }),
  ])
    assert.equal(await transport.admitted(msg, socket), null);
  assert.equal(journal.pending().length, 0);
});
test("heartbeat sends available on schedule and terminates composing; disconnected transports stay quiet", async (t) => {
  const { journal, auth, root } = fixture(t),
    transport = new WhatsAppSocket(config, journal, auth, root, () => {}),
    calls: unknown[][] = [];
  transport.socket = {
    sendPresenceUpdate: async (...args: unknown[]) => {
      calls.push(args);
    },
  } as unknown as WASocket;
  await transport.presence();
  assert.equal(calls.length, 0);
  transport.connected = true;
  await transport.presence();
  await transport.presence();
  assert.deepEqual(calls, [["available"]]);
  transport.setBusy([input.jid]);
  await transport.presence();
  assert.deepEqual(calls[1], ["composing", input.jid]);
  transport.setBusy([]);
  transport.lastOnlinePresenceAt = new Date(Date.now() - 120001).toISOString();
  await transport.presence();
  assert.deepEqual(calls.slice(2), [["paused", input.jid], ["available"]]);
});

test("read receipts follow durable sender admission and retry independently of native answers", async (t) => {
  const { journal, auth, root } = fixture(t),
    transport = new WhatsAppSocket(config, journal, auth, root, () => {});
  let attempts = 0;
  const socket = {
    readMessages: async () => {
      assert.equal(journal.pending().length, 1);
      if (++attempts === 1) throw Error("temporary");
    },
  } as unknown as WASocket;
  transport.socket = socket;
  transport.connected = true;
  const incoming = transport as unknown as {
    inbound(
      message: WAMessage,
      socket: WASocket,
      generation: number,
    ): Promise<void>;
  };
  const message: WAMessage = {
    key: { id: "receipt-input", remoteJid: input.jid, fromMe: false },
    messageTimestamp: Math.floor(Date.now() / 1000),
    message: { conversation: "hi" },
  };
  await incoming.inbound(
    {
      ...message,
      key: { ...message.key, remoteJid: "15555550999@s.whatsapp.net" },
    },
    socket,
    0,
  );
  await transport.flushReadReceipts();
  assert.equal(attempts, 0);
  await incoming.inbound(message, socket, 0);
  assert.equal(attempts, 0);
  await transport.flushReadReceipts();
  assert.equal(transport.errorCode, "whatsapp_read_receipt_failed");
  const restored = new WhatsAppSocket(config, journal, auth, root, () => {});
  restored.socket = socket;
  restored.connected = true;
  await restored.flushReadReceipts();
  await restored.flushReadReceipts();
  assert.equal(attempts, 2);
  assert.equal(journal.pending().length, 1);
});
test("commands never select a supplied task; fast and commentary persist without injecting a prompt", async (t) => {
  const { journal } = fixture(t);
  let created = 0;
  const bridge = {
    main: {
      create: async () => {
        created++;
      },
    },
  } as unknown as ChatBridge;
  const commands = new WhatsAppCommands(
    bridge,
    journal,
    {} as WhatsAppNative,
    async () => "desktop",
  );
  const row = { ...input, state: "pending", request: null };
  assert.deepEqual(parseCommand("/steer hello\nworld"), {
    name: "steer",
    args: "hello\nworld",
  });
  await assert.rejects(commands.run(row, "/new arbitrary-task"));
  assert.equal(created, 0);
  assert.match(
    (await commands.run(row, "/switch arbitrary-task"))!,
    /only one Main/,
  );
  await commands.run(row, "/fast on");
  await commands.run(row, "/commentary on");
  assert.equal(commands.preferences().serviceTier, "fast");
  assert.equal(commands.preferences().commentary, true);
  await commands.run(row, "/fast AUS");
  await commands.run(row, "/commentary 0");
  assert.equal(commands.preferences().serviceTier, null);
  assert.equal(commands.preferences().commentary, false);
  await commands.run(row, "/reasoning HIGH");
  assert.equal(commands.preferences().effort, "high");
  await commands.run(row, "/reasoning reset");
  assert.equal(commands.preferences().effort, null);
  assert.equal(
    commandText({ key: {}, message: { imageMessage: { caption: "/new" } } }),
    "",
  );
  assert.equal(
    commandText({
      key: {},
      message: { documentMessage: { caption: "/model reset" } },
    }),
    "",
  );
  assert.equal(
    commandText({ key: {}, message: { conversation: "/new" } }),
    "/new",
  );
  assert.equal(whatsappText('::created-thread{threadId="x"}'), "Task created.");
  assert.equal(whatsappText('Done\n::created-thread{threadId="x"}'), "Done");
  assert.equal(
    delegationSource({
      type: "userMessage",
      content: [
        {
          type: "text",
          text: "<codex_delegation><source_thread_id>source</source_thread_id></codex_delegation>",
        },
      ],
    }),
    "source",
  );
});

test("WhatsApp Main creation and direct input use the current runtime epoch", async (t) => {
  const epoch = "current-runtime",
    client = {
      request: async (method: string) => {
        assert.equal(method, "system.status");
        return { runtimeEpoch: epoch };
      },
    };
  const { journal } = fixture(t);
  let created: Chat.CreateMainRequest | null = null;
  const bridge = {
    main: {
      find: async () => null,
      admission: { definitionHash: 'partition', expected: () => ({}) },
      store: { client },
      create: async (_sender: string, request: Chat.CreateMainRequest) => {
        created = request;
        return { phase: "failed", error: { code: "fixture" } };
      },
    },
  } as unknown as ChatBridge;
  const commands = new WhatsAppCommands(
      bridge,
      journal,
      {} as WhatsAppNative,
      async () => "desktop",
    ),
    row = { ...input, state: "pending", request: null };
  assert.equal(
    await commands.run(row, "/new"),
    "Main could not be replaced: fixture",
  );
  assert.equal(parseOperationId(created!.operationId).runtimeEpoch, epoch);

  const root = mkdtempSync(join(tmpdir(), "ivy-wa-operation-")),
    runtime = new WhatsAppRuntime(config, root, "host", {} as Chat.Definition);
  t.after(async () => {
    await runtime.close();
    rmSync(root, { recursive: true, force: true });
  });
  const inbound = {
    ...input,
    id: "direct",
    message: encode({
      key: { id: "direct" },
      message: { conversation: "hello" },
    }),
  };
  runtime.journal.accept(inbound);
  let sent: Chat.SendRequest | null = null,
    ensured: string | null = null,
    invalidated = 0;
  const internal = runtime as unknown as {
    attached: unknown;
    input(): Promise<void>;
  };
  internal.attached = {
    bridge: {
      main: {
        admission: { definitionHash: 'partition', expected: () => ({}) },
        store: { client },
        ensure: async (_sender: string, operationId: string) => {
          ensured = operationId;
        },
      },
    },
    commands: { turnOptions: () => ({}) },
    native: {
      invalidate: () => {
        invalidated++;
      },
      bound: async () => ({
        main: { value: { binding: { objectId: "binding", revision: 1 } } },
      }),
      sameHost: async () => false,
      send: async (_sender: string, request: Chat.SendRequest) => {
        sent = request;
        return null;
      },
    },
  };
  await internal.input();
  assert.equal(parseOperationId(sent!.operationId).runtimeEpoch, epoch);
  assert.equal(ensured, sent!.operationId);
  assert.equal(invalidated, 1);
  assert.equal(
    parseOperationId(
      (JSON.parse(runtime.journal.pending()[0]!.request!) as Chat.SendRequest)
        .operationId,
    ).runtimeEpoch,
    epoch,
  );
});

test("temporary service failures retain WhatsApp input for a later dispatch", async (t) => {
  const { journal } = fixture(t);
  journal.accept(input);
  journal.saveRequest(input.id, { messageId: input.id });
  journal.set("direct-input:" + input.id, true);
  let completed = 0;
  const run = WhatsAppRuntime.prototype as unknown as {
    input(this: unknown): Promise<void>;
  };
  for (const code of [
    "service_unavailable",
    "service_not_ready",
    "storage_unavailable",
    "deadline_exceeded",
    "not_found",
  ]) {
    const runtime = {
      attached: {
        native: {
          send: async () => {
            throw new IvyError(code, "Temporary");
          },
        },
      },
      journal,
      text: () => "",
      complete: () => {
        completed++;
      },
    };
    await run.input.call(runtime);
    assert.equal(completed, 0);
    assert.equal(journal.pending().length, 1);
  }
});

test('expired Codex authentication retains the original WhatsApp input and emits only one sign-in notice', async t => {
  const { journal } = fixture(t), request = { messageId: input.id, payload: { text: 'Keep me', images: [] } };
  journal.accept(input); journal.saveRequest(input.id, request); journal.set('direct-input:' + input.id, true);
  let blocked = true;
  const runtime = { attached: { native: { send: async () => {
    if (blocked) throw new IvyError('chat_auth_required', 'Sign in');
    return {};
  } } }, journal, text: () => '', t: translator('de'), run: () => {}, complete: (row: {id:string}) => journal.finish(row.id) };
  const run = WhatsAppRuntime.prototype as unknown as { input(this: unknown): Promise<void> };
  for (let i = 0; i < 2; i++) await assert.rejects(run.input.call(runtime), { code: 'chat_auth_required' });
  assert.equal(journal.pending().length, 1); assert.deepEqual(JSON.parse(journal.pending()[0]!.request!), request);
  assert.equal(journal.outgoing().length, 1); assert.match(journal.outgoing()[0]!.text, /Codex-Anmeldung/);
  blocked = false; await run.input.call(runtime);
  assert.equal(journal.pending().length, 0); assert.equal(journal.outgoing().length, 1);
});

test('failed Main turns expose the cause and retain an already queued terminal notice across updates', async t => {
  const { journal } = fixture(t), binding = { pin: { objectId: 'binding' }, value: { primary: { nativeId: 'main' } } };
  const turn = { id: 'failed', status: 'failed', error: { message: 'workspace routing discovery unauthorized (401)' }, itemsView: 'full', items: [] };
  const native = { bound: async () => ({ binding }), turnPage: async () => ({ data: [turn], nextCursor: null }) } as unknown as WhatsAppNative;
  journal.set('listener-new:binding', true);
  const listener = new WhatsAppListener({} as ChatBridge, journal, native, config, translator('de'));
  await listener.tick(false); await listener.tick(false);
  assert.equal(journal.outgoing().length, 1); assert.match(journal.outgoing()[0]!.text, /Codex-Anmeldung.*abgelaufen/);
  const original = journal.outgoing()[0]!.text;
  const reopened = new WhatsAppListener({} as ChatBridge, journal, native, config, translator());
  await reopened.tick(false); assert.equal(journal.outgoing()[0]!.text, original);
  turn.id = 'legacy';
  const key = `terminal:binding:legacy:${config.senders[0]!.channelId}`;
  const ids = journal.enqueue(key, input.jid, 'Main turn ended: failed. No final text response.');
  await reopened.tick(false); assert.equal(journal.sent(ids), false);
  for (const id of ids) journal.mark(id, 'sent');
  await reopened.tick(false); assert.equal(journal.get('turn-delivered:binding:legacy'), true);
  assert.equal(terminalText({ status: 'failed', error: { message: 'Model is unavailable' } }), 'Main could not answer: Model is unavailable');
  assert.ok(terminalText({ status: 'failed', error: { message: 'x'.repeat(10000) } }).length < 700);
});

test("same-host images carry bytes across native user boundaries without a Hive blob round trip", async (t) => {
  const { journal, root } = fixture(t),
    bridge = {
      main: {
        store: {
          client: {
            request: async () => {
              throw Error("Unexpected Hive media write");
            },
          },
        },
      },
    } as unknown as ChatBridge;
  const image = Buffer.alloc(3 * 1024 * 1024);
  image.set([255, 216, 255]);
  for (const [id, bytes, content] of [
    [
      "image",
      image,
      {
        imageMessage: {
          mimetype: "image/jpeg",
          fileLength: image.length,
          caption: "/new",
        },
      },
    ],
    [
      "document",
      Buffer.from("Document"),
      { documentMessage: { fileName: "notes.txt", fileLength: 8 } },
    ],
  ] as const) {
    const directory = join(root, "whatsapp-media", id);
    mkdirSync(directory, { recursive: true });
    writeFileSync(join(directory, "original"), bytes);
    const payload = await mediaPayload(
      {
        ...input,
        id,
        message: encode({ key: { id }, message: content }),
        state: "pending",
        request: null,
      },
      bridge,
      journal,
      config,
      root,
      true,
    );
    assert.deepEqual(payload.images, []);
    const local = journal.get<
      Array<{ type: string; path?: string; name?: string; url?: string }>
    >("local-input:" + id)!;
    if (id === "image") {
      assert.equal(payload.text, "/new");
      assert.equal(local[0]!.type, "image");
      assert.equal(local[0]!.path, undefined);
      assert.deepEqual(Buffer.from(local[0]!.url!.split(",")[1]!, "base64"), bytes);
    } else {
      assert.equal(local[0]!.type, "mention");
      assert.ok(local[0]!.path!.startsWith(directory));
      assert.equal(local[0]!.name, "notes.txt");
    }
  }
  await assert.rejects(mediaPayload({ ...input, id: "oversized-image", state: "pending", request: null,
    message: encode({ key: { id: "oversized-image" }, message: { imageMessage: {
      mimetype: "image/jpeg", fileLength: 4 * 1024 * 1024 + 1,
    } } }) }, bridge, journal, config, root, true), { code: "whatsapp_media_limit" });
});

test("WhatsApp image downloads select the original direct path and validate its checksum", async (t) => {
  const { journal, root } = fixture(t);
  const original = Buffer.from("original image bytes"),
    mediaKey = Buffer.alloc(32, 1),
    expanded = Buffer.from(
      hkdfSync("sha256", mediaKey, Buffer.alloc(0), "WhatsApp Image Keys", 112),
    ),
    iv = expanded.subarray(0, 16),
    key = expanded.subarray(16, 48),
    macKey = expanded.subarray(48, 80);
  const encrypted = (bytes: Buffer) => {
    const cipher = createCipheriv("aes-256-cbc", key, iv);
    const ciphertext = Buffer.concat([cipher.update(bytes), cipher.final()]);
    const mac = createHmac("sha256", macKey)
      .update(iv)
      .update(ciphertext)
      .digest()
      .subarray(0, 10);
    return Buffer.concat([ciphertext, mac]);
  };
  let download = encrypted(original);
  const fetch = t.mock.method(
    globalThis,
    "fetch",
    async (url: string | URL | Request) => {
      assert.equal(String(url), "https://mmg.whatsapp.net/original");
      return new Response(new Uint8Array(download));
    },
  );
  for (const valid of [true, false]) {
    const id = valid ? "original-download" : "corrupt-download";
    if (!valid) download = encrypted(Buffer.alloc(original.length));
    const payload = mediaPayload(
      {
        ...input,
        id,
        state: "pending",
        request: null,
        message: encode(
          proto.WebMessageInfo.fromObject({
            key: { id },
            message: {
              ephemeralMessage: {
                message: {
                  imageMessage: {
                    directPath: "/original",
                    thumbnailDirectPath: "/thumbnail",
                    mediaKey,
                    mimetype: "image/jpeg",
                    fileLength: original.length,
                    fileSha256: createHash("sha256").update(original).digest(),
                    caption: "Image caption",
                  },
                },
              },
            },
          }),
        ),
      },
      {} as ChatBridge,
      journal,
      config,
      root,
      true,
    );
    if (valid) {
      assert.deepEqual(await payload, { text: "Image caption", images: [] });
      assert.equal(
        journal.get<Array<{ type: string }>>("local-input:" + id)?.[0]?.type,
        "image",
      );
    } else {
      await assert.rejects(
        payload,
        (error: unknown) =>
          error instanceof IvyError && error.code === "whatsapp_media_invalid",
      );
      assert.equal(journal.get("local-input:" + id), null);
    }
  }
  assert.equal(fetch.mock.callCount(), 2);
});

test("remote WhatsApp images belong to the retained Main conversation", async (t) => {
  const { journal, root } = fixture(t);
  const bytes = Buffer.from([255, 216, 255, 1]);
  const directory = join(root, "whatsapp-media", "remote-image");
  mkdirSync(directory, { recursive: true });
  writeFileSync(join(directory, "original"), bytes);
  let writes = 0;
  const bridge = {
    main: {
      queue: {
        main: async () => ({
          value: { conversation: { objectId: "conversation" } },
        }),
      },
      store: {
        rootObjectId: null,
        client: {
          request: async (method: string, params: Record<string, unknown>) => {
            if (method === "system.status")
              return { runtimeEpoch: "media-test" };
            assert.equal(method, "objects.write");
            assert.equal(
              (params["create"] as { ownerObjectId: string }).ownerObjectId,
              "conversation",
            );
            writes++;
            return { object: { id: "image" }, revision: { revision: 1 } };
          },
        },
      },
    },
  } as unknown as ChatBridge;
  const payload = await mediaPayload(
    {
      ...input,
      id: "remote-image",
      state: "pending",
      request: null,
      message: encode({
        key: { id: "remote-image" },
        message: {
          imageMessage: { mimetype: "image/jpeg", fileLength: bytes.length },
        },
      }),
    },
    bridge,
    journal,
    config,
    root,
    false,
  );
  assert.equal(writes, 1);
  assert.equal(payload.images[0]?.object.objectId, "image");
});

test("a transcribed voice retry uses the IvyChat voice marker without trusting inconsistent WhatsApp media metadata", async (t) => {
  const { journal, root } = fixture(t),
    id = "voice-retry",
    bytes = Buffer.from("previously-decoded-opus");
  const directory = join(root, "whatsapp-media", id);
  mkdirSync(directory, { recursive: true });
  writeFileSync(join(directory, "original"), bytes);
  journal.set("transcript:" + id, "Hallo aus WhatsApp.");
  const voiceConfig: WhatsAppConfig = {
    ...config,
    transcription: {
      apiKey: "test",
      baseUrl: "https://api.openai.com/v1",
      model: "gpt-4o-mini-transcribe",
    },
  };
  const payload = await mediaPayload(
    {
      ...input,
      id,
      message: encode({
        key: { id },
        message: {
          audioMessage: {
            mimetype: "audio/ogg; codecs=opus",
            fileLength: bytes.length,
            fileSha256: Buffer.alloc(32),
            seconds: 0,
            ptt: true,
          },
        },
      }),
      state: "pending",
      request: null,
    },
    {} as ChatBridge,
    journal,
    voiceConfig,
    root,
    false,
  );
  assert.deepEqual(payload, {
    text: "Voice message (automatically transcribed):\nHallo aus WhatsApp.",
    images: [],
  });
});

test("WhatsApp locales are instance-local with English default, German selection, interpolation and per-key fallback", async (t) => {
  const en = translator(),
    de = translator("de");
  assert.match(en("help"), /one shared Main/);
  assert.match(de("help"), /ein gemeinsamer Main/);
  assert.equal(
    de("media.attachment", { name: "Bericht.pdf" }),
    "Dateianhang: Bericht.pdf",
  );
  assert.equal(
    de("desktop.platform"),
    "Desktop commands require the Windows host.",
  );
  assert.doesNotMatch(de("desktop.platform"), /desktop\.platform/);
  assert.equal(
    whatsappText('::created-thread{threadId="x"}', de),
    "Task angelegt.",
  );
  assert.equal(
    whatsappText('::created-thread{threadId="x"}', en),
    "Task created.",
  );
  const first = fixture(t),
    second = fixture(t),
    row = { ...input, state: "pending", request: null };
  const english = new WhatsAppCommands(
    {} as ChatBridge,
    first.journal,
    {} as WhatsAppNative,
    async () => "desktop",
    en,
  );
  const german = new WhatsAppCommands(
    {} as ChatBridge,
    second.journal,
    {} as WhatsAppNative,
    async () => "desktop",
    de,
  );
  assert.match((await english.run(row, "/help"))!, /one shared Main/);
  assert.match((await german.run(row, "/help"))!, /ein gemeinsamer Main/);
  assert.match((await english.run(row, "/unknown"))!, /Unknown command/);
  assert.match((await german.run(row, "/unknown"))!, /Unbekannter Befehl/);
  const unsupported = {
    ...input,
    id: "unsupported",
    message: encode({
      key: { id: "unsupported" },
      message: { videoMessage: {} },
    }),
    state: "pending",
    request: null,
  };
  await assert.rejects(
    mediaPayload(
      unsupported,
      {} as ChatBridge,
      second.journal,
      config,
      second.root,
      false,
      de,
    ),
    (error) =>
      error instanceof IvyError && error.message.includes("Bitte Text"),
  );
});

test("WhatsApp retries keep their message ID and unresolved delivery cannot block subsequent replies forever", async (t) => {
  const { journal, auth, root } = fixture(t),
    transport = new WhatsAppSocket(config, journal, auth, root, () => {}),
    ids: string[] = [];
  transport.connected = true;
  transport.socket = {
    sendMessage: async (
      _jid: unknown,
      _body: unknown,
      options: { messageId: string },
    ) => {
      ids.push(options.messageId);
      throw Error("connection");
    },
  } as unknown as WASocket;
  const [first] = journal.enqueue("first", input.jid, "first");
  journal.enqueue("next", input.jid, "next");
  for (let attempt = 0; attempt < 3; attempt++) {
    journal.set("send-attempt:" + first, { count: attempt, at: 0 });
    await transport.flush();
  }
  assert.deepEqual(ids, [first, first, first]);
  transport.socket = {
    sendMessage: async (_jid: unknown, body: { text: string }) => {
      assert.equal(body.text, "next");
      return {};
    },
  } as unknown as WASocket;
  await transport.flush();
  assert.equal(journal.outgoing()[0]!.state, "unknown");
  const parts = journal.enqueue("chunks", input.jid, "😀".repeat(7001));
  let chunks = 0;
  transport.socket = {
    sendMessage: async () => {
      chunks++;
      return {};
    },
  } as unknown as WASocket;
  await transport.flush();
  assert.equal(chunks, 3);
  assert.equal(journal.sent(parts), true);
});

test("transport retention keeps pending work and current-turn deduplication, and commits memory transactions atomically", async (t) => {
  const { journal, auth, root } = fixture(t),
    old = Date.now() - 8 * 86400_000;
  journal.accept({ ...input, received: old });
  auth.set("creds", "default", "retained");
  journal.set("preferences", { model: "chosen" });
  assert.throws(() =>
    journal.transaction(() => {
      journal.set("rolled-back", true);
      throw Error("rollback");
    }),
  );
  const [id] = journal.enqueue("old", input.jid, "current turn");
  journal.mark(id!, "sent");
  journal.set("out-created:" + id, old);
  journal.set("message:binding:turn:item:sender", {
    hash: "hash",
    ids: [id],
    suppressed: false,
    at: old,
  });
  journal.set("listener:binding", { lastTurn: "turn" });
  journal.prune();
  assert.equal(journal.pending().length, 1);
  assert.equal(journal.sent([id!]), true);
  journal.set("listener:binding", { lastTurn: "new" });
  journal.prune();
  assert.equal(journal.get("message:binding:turn:item:sender"), null);
  await journal.close();
  auth.close();
  const reopened = new WhatsAppJournal(join(root, "journal.sqlite")),
    reopenedAuth = new WhatsAppAuthStore(join(root, "auth.sqlite"));
  try {
    assert.equal(reopened.get("rolled-back"), null);
    assert.equal(reopened.pending().length, 1);
    assert.equal(reopenedAuth.get("creds", "default"), "retained");
    assert.deepEqual(reopened.get("preferences"), { model: "chosen" });
  } finally {
    await reopened.close();
    reopenedAuth.close();
  }
});

test("a cleared transport journal does not remove WhatsApp credentials", async (t) => {
  const { journal, auth, root } = fixture(t);
  journal.accept(input);
  auth.set("creds", "default", "retained");
  await journal.close();
  auth.close();
  rmSync(join(root, "journal.sqlite"), { force: true });
  rmSync(join(root, "journal.sqlite-wal"), { force: true });
  rmSync(join(root, "journal.sqlite-shm"), { force: true });
  const empty = new WhatsAppJournal(join(root, "journal.sqlite")),
    retained = new WhatsAppAuthStore(join(root, "auth.sqlite"));
  try {
    assert.equal(empty.pending().length, 0);
    assert.equal(retained.get("creds", "default"), "retained");
  } finally {
    await empty.close();
    retained.close();
  }
});

test("listener baselines only the newest turn and catches up only to the previous watermark", async (t) => {
  const { journal } = fixture(t),
    binding = {
      pin: { objectId: "binding" },
      value: { primary: { nativeId: "main" } },
    };
  let fresh = false,
    calls = 0;
  const turn = (id: string) => ({
    id,
    status: "completed",
    itemsView: "full",
    items: [{ id: "item", type: "agentMessage", text: id }],
  });
  const native = {
    bound: async () => ({ binding }),
    turnPage: async (_threadId: string, cursor: string | null) => {
      calls++;
      assert.equal(
        cursor,
        fresh && cursor !== null ? 'old-turn' : null,
        "Never traverse historical pages beyond the watermark",
      );
      return {
        data: fresh && cursor === null ? [turn('new')] : [turn('old')],
        nextCursor: fresh && cursor === null ? 'old-turn' : 'older-history',
      };
    },
  } as unknown as WhatsAppNative;
  const listener = new WhatsAppListener(
    {} as ChatBridge,
    journal,
    native,
    config,
  );
  await listener.tick(false);
  assert.equal(journal.outgoing().length, 0);
  fresh = true;
  await listener.tick(false);
  await listener.tick(false);
  assert.equal(calls, 4);
  assert.deepEqual(
    journal.outgoing().map((row) => row.text),
    ["new"],
  );
});
test("WhatsApp fallback never overwrites explicit model and effort, including after reopening the journal", async (t) => {
  const { journal, root } = fixture(t);
  const commands = new WhatsAppCommands(
    {} as ChatBridge,
    journal,
    {} as WhatsAppNative,
    async () => "desktop",
  );
  assert.deepEqual(commands.turnOptions(), {
    model: "gpt-5.6-terra",
    effort: "medium",
  });
  assert.equal(journal.get("preferences"), null);
  journal.set("preferences", {
    model: "gpt-5.6-luna",
    effort: "high",
    commentary: true,
    serviceTier: "fast",
  });
  assert.deepEqual(commands.turnOptions(), {
    model: "gpt-5.6-luna",
    effort: "high",
    serviceTier: "fast",
  });
  await journal.close();
  const reopened = new WhatsAppJournal(join(root, "journal.sqlite"));
  try {
    const restored = new WhatsAppCommands(
      {} as ChatBridge,
      reopened,
      {} as WhatsAppNative,
      async () => "desktop",
    );
    assert.deepEqual(restored.turnOptions(), {
      model: "gpt-5.6-luna",
      effort: "high",
      serviceTier: "fast",
    });
    await restored.run(
      { ...input, state: "pending", request: null },
      "/commentary off",
    );
    assert.deepEqual(restored.turnOptions(), {
      model: "gpt-5.6-luna",
      effort: "high",
      serviceTier: "fast",
    });
  } finally {
    await reopened.close();
  }
});
test("typing follows native turn activity and never starts merely from queued input", async (t) => {
  const root = mkdtempSync(join(tmpdir(), "ivy-wa-activity-")),
    runtime = new WhatsAppRuntime(config, root, "host", {} as Chat.Definition),
    calls: unknown[][] = [];
  t.after(async () => {
    await runtime.close();
    rmSync(root, { recursive: true, force: true });
  });
  let queue: unknown[] = [{}];
  const internal = runtime as unknown as {
    attached: unknown;
    nativeBusy: boolean;
    nativeBusyAt: number;
    activity(): Promise<void>;
  };
  internal.attached = {
    bridge: { main: { find: async () => ({ value: { queue } }) } },
  };
  runtime.transport.connected = true;
  runtime.transport.socket = {
    sendPresenceUpdate: async (...args: unknown[]) => {
      calls.push(args);
    },
    end: () => {},
  } as unknown as WASocket;
  await internal.activity();
  assert.ok(!calls.some((call) => call[0] === "composing"));
  internal.nativeBusy = true;
  internal.nativeBusyAt = Date.now();
  await internal.activity();
  assert.ok(
    calls.some((call) => call[0] === "composing" && call[1] === input.jid),
  );
  internal.nativeBusy = false;
  await internal.activity();
  assert.ok(
    calls.some((call) => call[0] === "paused" && call[1] === input.jid),
  );
});
test("a Main change clears a stale one-shot native event error", async (t) => {
  const root = mkdtempSync(join(tmpdir(), "ivy-wa-context-")),
    runtime = new WhatsAppRuntime(config, root, "host", {} as Chat.Definition);
  t.after(async () => {
    await runtime.close();
    rmSync(root, { recursive: true, force: true });
  });
  const internal = runtime as unknown as { errors: Map<string, string> };
  internal.errors.set("native-events", "chat_main_missing");
  runtime.changed();
  assert.equal(internal.errors.has("native-events"), false);
});
test("independent Main listener delivers multiple finals and later same-turn answers once, including with an empty Chat queue", async (t) => {
  const { journal } = fixture(t);
  const binding = {
    pin: { objectId: "binding", revision: 1 },
    value: { primary: { nativeId: "main" } },
  };
  const bridge = {
    main: {
      find: async () => ({ value: { binding: binding.pin, queue: [] } }),
      store: { read: async () => binding },
    },
  } as unknown as ChatBridge;
  let items = [
    {
      type: "agentMessage",
      id: "comment",
      phase: "commentary",
      text: "Working",
    },
    {
      type: "agentMessage",
      id: "final",
      phase: "final_answer",
      text: "Answer",
    },
  ];
  const native = {
    bound: async () => ({ binding }),
    turnPage: async () => ({
      data: [{ id: "turn", status: "completed", itemsView: "full", items }],
      nextCursor: null,
    }),
  } as unknown as WhatsAppNative;
  journal.set("listener-new:binding", true);
  const listener = new WhatsAppListener(bridge, journal, native, config);
  await listener.tick(false);
  await listener.tick(false);
  assert.deepEqual(
    journal.outgoing().map((x) => x.text),
    ["Answer"],
  );
  for (const row of journal.outgoing()) journal.mark(row.id, "sent");
  await listener.tick(false);
  assert.equal(journal.get("turn-delivered:binding:turn"), true);
  items = [
    ...items,
    {
      type: "agentMessage",
      id: "later",
      phase: "final_answer",
      text: "Delayed result",
    },
  ];
  await listener.tick(false);
  await listener.tick(false);
  assert.deepEqual(
    journal.outgoing().map((x) => x.text),
    ["Delayed result"],
  );
});
test("existing Main establishes a baseline without replaying old history and never emits a partial last streaming message", async (t) => {
  const { journal } = fixture(t),
    binding = {
      pin: { objectId: "binding", revision: 1 },
      value: { primary: { nativeId: "main" } },
    };
  const bridge = {
    main: {
      find: async () => ({ value: { binding: binding.pin } }),
      store: { read: async () => binding },
    },
  } as unknown as ChatBridge;
  let status = "completed",
    items = [{ type: "agentMessage", id: "old", text: "History" }];
  const native = {
    bound: async () => ({ binding }),
    turnPage: async () => ({
      data: [{ id: "turn", status, itemsView: "full", items }],
      nextCursor: null,
    }),
  } as unknown as WhatsAppNative;
  const listener = new WhatsAppListener(bridge, journal, native, config);
  await listener.tick(false);
  assert.equal(journal.outgoing().length, 0);
  status = "inProgress";
  items = [...items, { type: "agentMessage", id: "stream", text: "partial" }];
  await listener.tick(false);
  assert.equal(journal.outgoing().length, 0);
  status = "completed";
  items[1]!.text = "complete";
  await listener.tick(false);
  assert.equal(journal.outgoing()[0]!.text, "complete");
});

test("native events deliver completed Main messages immediately and history recovery does not duplicate them", async (t) => {
  const { journal } = fixture(t),
    binding = {
      pin: { objectId: "binding", revision: 1 },
      value: { primary: { nativeId: "main" } },
    };
  const bridge = {
    main: {
      find: async () => ({ value: { binding: binding.pin } }),
      store: { read: async () => binding },
    },
  } as unknown as ChatBridge;
  const item = {
    type: "agentMessage",
    id: "final",
    phase: "final_answer",
    text: "Immediate answer",
  };
  const native = {
    bound: async () => ({ binding }),
    turnPage: async () => ({
      data: [
        { id: "turn", status: "completed", itemsView: "full", items: [item] },
      ],
      nextCursor: null,
    }),
  } as unknown as WhatsAppNative;
  const listener = new WhatsAppListener(bridge, journal, native, config);
  journal.set("listener-new:binding", true);
  const event = (method: string, params: unknown) =>
    ({ method, params }) as Agent.Notification;
  assert.equal(
    await listener.notification(
      event("turn/started", { threadId: "other" }),
      false,
    ),
    null,
  );
  assert.deepEqual(
    await listener.notification(
      event("turn/started", { threadId: "main" }),
      false,
    ),
    { busy: true },
  );
  await listener.notification(
    event("item/agentMessage/delta", {
      threadId: "main",
      turnId: "turn",
      delta: "partial",
    }),
    false,
  );
  assert.equal(journal.outgoing().length, 0);
  await listener.notification(event('item/completed', { threadId: 'main', turnId: 'turn', item: { type: 'userMessage', content: [] } }), false);
  await listener.notification(
    event("item/completed", { threadId: "main", turnId: "turn", item }),
    false,
  );
  assert.deepEqual(
    journal.outgoing().map((row) => row.text),
    ["Immediate answer"],
  );
  await listener.tick(false);
  assert.equal(journal.outgoing().length, 1);
  assert.deepEqual(
    await listener.notification(
      event("turn/completed", { threadId: "main" }),
      false,
    ),
    { busy: false },
  );
});

test("WhatsApp reply collection uses the current runtime epoch", async (t) => {
  const { journal } = fixture(t);
  let received: Chat.ReceiveRequest | null = null;
  const bridge = {
    main: {
      admission: { definitionHash: 'partition', expected: () => ({}) },
      store: {
        client: { request: async () => ({ runtimeEpoch: "outbox-runtime" }) },
      },
    },
    outbox: {
      history: async () => ({
        throughSequence: 1,
        entries: [{ data: { origin: { kind: "notice" } } }],
      }),
      receive: async (_sender: string, request: Chat.ReceiveRequest) => {
        received = request;
        return { phase: "failed" };
      },
    },
  } as unknown as ChatBridge;
  const listener = new WhatsAppListener(
    bridge,
    journal,
    {} as WhatsAppNative,
    config,
  );
  await listener.outbox();
  assert.equal(
    parseOperationId(received!.operationId).runtimeEpoch,
    "outbox-runtime",
  );
  assert.equal(
    parseOperationId(
      journal.get<{ request: Chat.ReceiveRequest }>("offer:partition:main-wa")!.request
        .operationId,
    ).runtimeEpoch,
    "outbox-runtime",
  );
});

test("native reply acknowledgement reads the exact result under its input and waits for WhatsApp delivery", async (t) => {
  const { journal } = fixture(t);
  let acknowledged = 0,
    acknowledgement: Chat.AcknowledgeRequest | null = null;
  const reply = {
    object: { objectId: "reply", revision: 1 },
    data: {
      origin: {
        kind: "native",
        inputId: "original-input",
        result: { objectId: "result", revision: 1 },
        binding: { objectId: "binding", revision: 1 },
      },
    },
  };
  const offer = {
    object: { objectId: "offer", revision: 1 },
    data: { throughSequence: 3 },
  };
  const bridge = {
    main: {
      operations: { find: async () => ({}) },
      admission: { definitionHash: 'partition', expected: () => ({}) },
      store: {
        client: { request: async () => ({ runtimeEpoch: "listener-runtime" }) },
        isNoticeInput: () => false,
        read: async (key: string, pin: unknown, parent: string) => {
          assert.equal(key, "chat-bridge/result");
          assert.deepEqual(pin, reply.data.origin.result);
          assert.equal(parent, "original-input");
          return { value: { turnId: "turn" } };
        },
      },
    },
    outbox: {
      acknowledge: async (
        _sender: string,
        request: Chat.AcknowledgeRequest,
      ) => {
        acknowledged++;
        acknowledgement = request;
        return { phase: "succeeded" };
      },
    },
  } as unknown as ChatBridge;
  journal.set("offer:partition:main-wa", { request: {}, offer, replies: [reply] });
  const listener = new WhatsAppListener(
    bridge,
    journal,
    {} as WhatsAppNative,
    config,
  );
  await listener.outbox();
  assert.equal(acknowledged, 0);
  const ids = journal.enqueue("late-turn", input.jid, "Late acknowledgement");
  journal.set("turn-delivered:binding:turn", ids);
  journal.set("listener:binding", { lastTurn: "newer-turn" });
  await listener.outbox();
  assert.equal(acknowledged, 0);
  for (const id of ids) journal.mark(id, "sent");
  await listener.outbox();
  assert.equal(acknowledged, 1);
  assert.equal(journal.get("turn-delivered:binding:turn"), true);
  assert.equal(
    parseOperationId(acknowledgement!.operationId).runtimeEpoch,
    "listener-runtime",
  );
  assert.equal(journal.get("offer:partition:main-wa"), null);
  assert.equal(journal.get("reply-cursor:partition:main-wa"), 3);
});

test("an expired WhatsApp offer is renewed without resending its already delivered reply", async (t) => {
  const journal = new WhatsAppJournal(":memory:");
  t.after(() => journal.close());
  const reply = {
    object: { objectId: "reply", revision: 1 },
    data: { origin: { kind: "notice" }, text: "Delivered", sequence: 3 },
  };
  const outgoing = journal.enqueue("notice:reply", input.jid, "Delivered");
  journal.mark(outgoing[0]!, "sent");
  journal.set("offer:partition:main-wa", {
    request: { operationId: "expired" },
    offer: { object: { objectId: "expired-offer" } },
    replies: [reply],
  });
  let acknowledgements = 0;
  const bridge = {
    main: {
      operations: { find: async () => null },
      admission: { definitionHash: 'partition', expected: () => ({}) },
      store: {
        client: { request: async () => ({ runtimeEpoch: "new-epoch" }) },
      },
    },
    outbox: {
      history: async () => ({ entries: [reply], throughSequence: 3 }),
      receive: async () => ({
        phase: "succeeded",
        outcome: {
          action: "receive",
          replies: [reply],
          offer: {
            object: { objectId: "new-offer", revision: 1 },
            data: { throughSequence: 3 },
          },
        },
      }),
      acknowledge: async (
        _caller: string,
        request: Chat.AcknowledgeRequest,
      ) => {
        assert.equal(request.offerId, "new-offer");
        acknowledgements++;
        return { phase: "succeeded" };
      },
    },
  } as unknown as ChatBridge;
  const listener = new WhatsAppListener(
    bridge,
    journal,
    {} as WhatsAppNative,
    config,
  );
  await listener.outbox();
  assert.equal(journal.get("offer:partition:main-wa"), null);
  await listener.outbox();
  assert.equal(acknowledgements, 1);
  assert.equal(journal.get("reply-cursor:partition:main-wa"), 3);
  assert.equal(
    journal.db.prepare("SELECT count(*) AS n FROM outgoing").get()!["n"],
    1,
  );
});
