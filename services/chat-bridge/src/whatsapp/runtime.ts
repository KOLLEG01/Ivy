import { join } from "node:path";
import { normalizeMessageContent } from "baileys";
import type { WAMessage } from "baileys";
import { IvyError, requireThat } from "../../../../packages/sdk/src/node.js";
import type {
  Agent,
  Chat,
  Transport,
} from "../../../../packages/sdk/src/node.js";
import { atomicJson } from "../../../../packages/host-runtime/src/config.js";
import type { ChatBridge } from "../bridge.js";
import { WhatsAppJournal } from "./journal.js";
import type { Inbox } from "./journal.js";
import { WhatsAppSocket } from "./socket.js";
import { WhatsAppNative } from "./native.js";
import { WhatsAppCommands, parseCommand } from "./commands.js";
import { WhatsAppListener } from "./listener.js";
import { readWhatsAppConfig, phone } from "./config.js";
import type { WhatsAppConfig } from "./config.js";
import { decode } from "./auth.js";
import { mediaPayload, commandText, pruneMedia } from "./media.js";
import { desktopCommand } from "./desktop.js";
import { ExecutorLock } from "../../../../packages/host-runtime/src/journal.js";
import { scopedOperationId } from "../../../../packages/sdk/src/client.js";
import { WhatsAppAuthStore } from "./auth.js";
import { translator } from "./messages.js";
import type { Translator, WhatsAppLanguage } from "./messages.js";
type Attached = {
  bridge: ChatBridge;
  native: WhatsAppNative;
  commands: WhatsAppCommands;
  listener: WhatsAppListener;
};
export const retryableWhatsAppError = (code: string) =>
  [
    "outcome_unknown",
    "service_not_ready",
    "service_unavailable",
    "storage_unavailable",
    "deadline_exceeded",
    "native_unavailable",
    "native_capacity",
    "stale_generation",
    "chat_pending_action",
    "revision_conflict",
    "native_read_pending",
    "not_found",
  ].includes(code);
export class WhatsAppRuntime {
  readonly journal: WhatsAppJournal;
  readonly authStore: WhatsAppAuthStore;
  readonly transport: WhatsAppSocket;
  private readonly lock: ExecutorLock;
  private attached: Attached | null = null;
  private timers: NodeJS.Timeout[] = [];
  private tasks = new Map<string, Promise<void>>();
  private again = new Map<string, () => Promise<void>>();
  private closed = false;
  private closing: Promise<void> | null = null;
  private errors = new Map<string, string>();
  private nativeBusy = false;
  private nativeBusyAt = 0;
  private nativeEventRevision = 0;
  private events: Agent.Notification[] = [];
  private lastNativeSequence = 0;
  private lastNativeEpoch: string | null = null;
  readonly t: Translator;
  constructor(
    readonly config: WhatsAppConfig,
    readonly dataRoot: string,
    readonly hostId: string,
    _definition: Chat.Definition,
    language: WhatsAppLanguage = "en",
  ) {
    this.t = translator(language);
    this.lock = new ExecutorLock(join(dataRoot, "whatsapp-owner"));
    this.journal = new WhatsAppJournal(join(dataRoot, "whatsapp.sqlite"));
    this.authStore = new WhatsAppAuthStore(
      join(dataRoot, "whatsapp", "auth.sqlite"),
    );
    // Configuration is validated on attach; the socket verifies the actual authenticated bot.
    // Changing a native plan must not invalidate unrelated WhatsApp credentials or pending input.
    this.transport = new WhatsAppSocket(
      config,
      this.journal,
      this.authStore,
      dataRoot,
      () => this.wake(),
    );
  }
  private wake(): void {
    this.run("control", () => this.control());
    this.run("input", () => this.input());
    this.run("read-receipts", () => this.transport.flushReadReceipts());
    this.run("output", () => this.transport.flush());
  }
  changed(): void {
    this.errors.delete("native-events");
    this.attached?.native.invalidate();
    this.outbox();
    this.run("listener", () => this.listen());
    this.wake();
  }
  private outbox(): void {
    const a = this.attached;
    if (a)
      this.run("chat-outbox", async () => {
        await a.listener.notices();
        await a.listener.outbox();
        this.run("output", () => this.transport.flush());
      });
  }
  static async open(
    path: string,
    dataRoot: string,
    hostId: string,
    definition: Chat.Definition,
    language: WhatsAppLanguage = "en",
  ): Promise<WhatsAppRuntime> {
    return new WhatsAppRuntime(
      await readWhatsAppConfig(path, definition),
      dataRoot,
      hostId,
      definition,
      language,
    );
  }
  attach(bridge: ChatBridge | null): void {
    this.errors.delete("native-events");
    this.lastNativeSequence = 0;
    this.lastNativeEpoch = null;
    if (!bridge) {
      this.attached = null;
      this.again.clear();
      this.events = [];
      this.nativeBusy = false;
      this.nativeEventRevision++;
      return;
    }
    const native = new WhatsAppNative(bridge, this.journal, this.t);
    this.attached = {
      bridge,
      native,
      commands: new WhatsAppCommands(
        bridge,
        this.journal,
        native,
        (action, key) => desktopCommand(action, key, this.journal, this.t),
        this.t,
      ),
      listener: new WhatsAppListener(
        bridge,
        this.journal,
        native,
        this.config,
        this.t,
      ),
    };
    this.run("listener", async () => {
      await native.prepare();
      await this.listen();
    });
    this.wake();
  }
  start(): void {
    this.transport.start();
    const every = (name: string, ms: number, fn: () => Promise<void>) => {
      this.timers.push(setInterval(() => this.run(name, fn), ms));
    };
    every("control", 1000, () => this.control());
    every("input", 1000, () => this.input());
    every("listener", 30000, () => {
      this.attached?.native.invalidate();
      return this.listen();
    });
    every("output", 5000, () => this.transport.flush());
    every("status", 10000, () => this.status());
    every("read-receipts", 1000, () => this.transport.flushReadReceipts());
    every("activity", 1000, () => this.activity());
    every("retention", 3600000, async () => {
      this.journal.prune();
      if (!this.nativeBusy) await pruneMedia(this.dataRoot, this.journal);
    });
  }
  private run(name: string, fn: () => Promise<void>): void {
    if (this.closed) return;
    if (this.tasks.has(name)) {
      this.again.set(name, fn);
      return;
    }
    const task = fn()
      .then(() => {
        this.errors.delete(name);
      })
      .catch((error) => {
        const code = IvyError.from(error).code;
        if (code === "native_read_pending") this.errors.delete(name);
        else this.errors.set(name, code);
      })
      .finally(() => {
        this.tasks.delete(name);
        const next = this.again.get(name);
        this.again.delete(name);
        if (next) this.run(name, next);
      });
    this.tasks.set(name, task);
  }
  private text(row: Inbox): string {
    return commandText(decode<WAMessage>(row.message));
  }
  private async control(): Promise<void> {
    const a = this.attached;
    if (!a) return;
    const pending = this.journal.pending();
    for (let index = 0; index < pending.length; index++) {
      const row = pending[index]!,
        command = parseCommand(this.text(row));
      if (!command) continue;
      // Steering/status bypass slow media or an executing turn; ordered setting/Main changes do not.
      if (
        ["new", "model", "reasoning", "fast"].includes(command.name) &&
        command.args !== "" &&
        index > 0
      )
        continue;
      if (command.name === "new" && index > 0) continue;
      try {
        const result = await a.commands.run(row, this.text(row));
        if (result !== null) this.complete(row, result);
      } catch (error) {
        const e = IvyError.from(error);
        if (["tool_definition_changed", "native_definition_mismatch", "native_owner_mismatch"].includes(e.code)) {
          a.native.invalidate();
          throw e;
        }
        if (retryableWhatsAppError(e.code)) continue;
        this.complete(
          row,
          this.t("command.failed", {
            code: e.code,
            detail: e.code === "invalid_arguments" ? " — " + e.message : "",
          }),
        );
      }
    }
  }
  private complete(row: Inbox, text?: string): void {
    this.journal.transaction(() => {
      if (text) this.journal.enqueue("response:" + row.id, row.jid, text);
      this.journal.finish(row.id);
    });
    this.run("output", () => this.transport.flush());
  }
  private async input(): Promise<void> {
    const a = this.attached;
    if (!a) return;
    const row = this.journal.pending()[0];
    if (!row || parseCommand(this.text(row))) return;
    try {
      let request = row.request
        ? (JSON.parse(row.request) as Chat.SendRequest)
        : null;
      if (!request) {
        const operationId = await scopedOperationId(
          a.bridge.main.store.client,
          ["whatsapp-send", row.id],
        );
        await a.bridge.main.ensure(row.sender, operationId);
        a.native.invalidate();
        const { main } = await a.native.bound();
        requireThat(
          main.value.binding,
          "chat_main_missing",
          this.t("main.pending"),
        );
        const sender = this.config.senders.find(
          (s) =>
            s.principalId === row.sender &&
            s.channelId === this.senderChannel(row),
        );
        requireThat(
          sender,
          "chat_sender_refused",
          "Sender no longer admitted.",
        );
        const content = normalizeMessageContent(
          decode<WAMessage>(row.message).message,
        );
        const sameHost =
          content?.documentMessage || content?.imageMessage
            ? await a.native.sameHost(this.hostId)
            : false;
        const payload = await mediaPayload(
          row,
          a.bridge,
          this.journal,
          this.config,
          this.dataRoot,
          sameHost,
          this.t,
        );
        const turnOptions = a.commands.turnOptions();
        request = {
          action: "send",
          operationId,
          messageId: row.id,
          expectedBridge: a.bridge.main.admission.expected(row.sender),
          expectedBinding: main.value.binding,
          channel: {
            adapter: "whatsapp",
            accountId: this.config.accountId,
            channelId: sender.channelId,
          },
          payload: { ...payload, turnOptions },
        };
        this.journal.transaction(() => {
          this.journal.set("direct-input:" + row.id, true);
          this.journal.saveRequest(row.id, request);
        });
      }
      if (this.journal.get<boolean>("direct-input:" + row.id)) {
        if ((await a.native.send(row.sender, request)) !== null)
          this.complete(row);
        return;
      }
      const result = await a.bridge.main.queue.send(row.sender, request);
      if (result.phase === "succeeded") this.complete(row);
      else if (result.phase === "failed")
        this.complete(
          row,
          this.t("message.rejected", { code: result.error?.code ?? "unknown" }),
        );
    } catch (error) {
      const e = IvyError.from(error);
      if (["tool_definition_changed", "native_definition_mismatch", "native_owner_mismatch"].includes(e.code)) {
        // Native delivery retains whether dispatch was rejected or uncertain.
        // Keep the original input for its guarded retry or receipt lookup.
        a.native.invalidate();
        throw e;
      }
      if (e.code === 'chat_auth_required') {
        const key = 'auth-required:' + row.id;
        if (!this.journal.hasOutput(key)) this.journal.enqueue(key, row.jid, this.t('message.auth_required'));
        this.run('output', () => this.transport.flush());
        throw e;
      }
      if (retryableWhatsAppError(e.code)) return;
      this.complete(
        row,
        e.code === "interaction_expired"
          ? this.t("message.expired")
          : e.code === 'chat_main_unavailable' ? this.t('main.unavailable')
          : this.t("message.failed", { code: e.code }),
      );
    }
  }
  private senderChannel(row: Inbox): string {
    const list = this.config.senders.filter(
      (s) => s.principalId === row.sender,
    );
    requireThat(
      list.length === 1,
      "chat_sender_refused",
      "Each WhatsApp sender needs one unique principal.",
    );
    return list[0]!.channelId;
  }
  private async listen(): Promise<void> {
    const a = this.attached;
    if (!a) return;
    const revision = this.nativeEventRevision;
    try {
      const busy = await a.listener.tick(a.commands.preferences().commentary);
      if (a === this.attached && revision === this.nativeEventRevision) {
        this.nativeBusy = busy;
        this.nativeBusyAt = Date.now();
        a.native.setBusy(busy);
      }
      this.run("output", () => this.transport.flush());
      if (a.listener.needsRecovery) this.run("listener", () => this.listen());
    } finally {
      this.outbox();
    }
  }
  private async activity(): Promise<void> {
    const busy =
      !!this.attached &&
      this.nativeBusy &&
      Date.now() - this.nativeBusyAt < 90000;
    this.transport.setBusy(
      busy
        ? this.config.senders.map(
            (s) => phone(s.phoneNumber) + "@s.whatsapp.net",
          )
        : [],
    );
    await this.transport.presence();
  }
  notification(value: Transport.ProviderNotification): void {
    const object = (v: unknown): Record<string, unknown> | null =>
      v !== null && typeof v === "object" && !Array.isArray(v)
        ? (v as Record<string, unknown>)
        : null;
    const params = value.params,
      event = object(params.payload),
      a = this.attached;
    if (
      !a ||
      params.serviceNodeId !==
        a.bridge.main.admission.definition.project.serviceNodeId ||
      event?.["serviceNodeId"] !== params.serviceNodeId ||
      event["nativeVersion"] !==
        a.bridge.main.admission.definition.nativePlan.nativeVersion
    )
      return;
    if (
      typeof event["sequence"] === "number" &&
      typeof event["epoch"] === "string"
    ) {
      const changed =
        this.lastNativeEpoch !== null &&
        this.lastNativeEpoch !== event["epoch"];
      if (
        changed ||
        (this.lastNativeSequence &&
          event["sequence"] > this.lastNativeSequence + 1)
      ) {
        a.native.invalidate(changed);
        this.run("listener", async () => {
          await a.native.prepare();
          await this.listen();
        });
      }
      this.lastNativeEpoch = event["epoch"];
      this.lastNativeSequence = event["sequence"];
    }
    if (
      !["turn/started", "turn/completed", "item/completed"].includes(
        String(event["method"]),
      ) ||
      !object(event["params"])
    )
      return;
    if (this.events.length >= 256) {
      this.events.shift();
      this.run("listener", () => this.listen());
    }
    this.events.push(event as unknown as Agent.Notification);
    this.run("native-events", async () => {
      while (this.events.length && this.attached) {
        const selected = this.attached,
          result = await selected.listener.notification(
            this.events.shift()!,
            selected.commands.preferences().commentary,
          );
        if (result && selected === this.attached) {
          if (result.busy !== null) {
            this.nativeEventRevision++;
            this.nativeBusy = result.busy;
            this.nativeBusyAt = Date.now();
            selected.native.setBusy(result.busy);
            this.run("activity", () => this.activity());
            if (!result.busy) this.wake();
          }
          this.run("output", () => this.transport.flush());
        }
      }
    });
  }
  async status(): Promise<void> {
    await atomicJson(join(this.dataRoot, "whatsapp-status.json"), {
      observedAt: new Date().toISOString(),
      state: this.transport.state,
      connected: this.transport.connected,
      online: this.transport.online,
      privacy: this.transport.privacy,
      lastOnlinePresenceAt: this.transport.lastOnlinePresenceAt,
      errorCode:
        this.transport.errorCode ?? this.errors.values().next().value ?? null,
      attached: !!this.attached,
      lastNativeDispatchMs: this.attached?.native.lastDispatchMs ?? null,
      lastWhatsAppSendMs: this.transport.lastSendMs,
      pendingInputs: this.journal.db
        .prepare("SELECT count(*) AS n FROM inbox WHERE state='pending'")
        .get()!["n"],
      pendingOutputs: this.journal.db
        .prepare("SELECT count(*) AS n FROM outgoing WHERE state<>'sent'")
        .get()!["n"],
    });
  }
  async settled(): Promise<void> {
    await Promise.allSettled(this.tasks.values());
  }
  close(): Promise<void> {
    return (this.closing ??= (async () => {
      this.closed = true;
      this.again.clear();
      for (const timer of this.timers) clearInterval(timer);
      try {
        try {
          await this.transport.close();
        } finally {
          await this.settled();
        }
      } finally {
        try {
          await this.journal.close();
        } finally {
          this.authStore.close();
          this.lock.close();
        }
      }
    })());
  }
}
