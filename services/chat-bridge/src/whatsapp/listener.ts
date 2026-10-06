import type { Agent, Chat, Wire } from "../../../../packages/sdk/src/node.js";
import { requireThat } from "../../../../packages/sdk/src/node.js";
import type { ChatBridge } from "../bridge.js";
import { identity, WhatsAppJournal } from "./journal.js";
import type { WhatsAppNative } from "./native.js";
import type { WhatsAppConfig } from "./config.js";
import { scopedOperationId } from "../../../../packages/sdk/src/client.js";
import { phone } from "./config.js";
import { translator } from "./messages.js";
import type { Translator } from "./messages.js";
import { mutation } from '../store.js';
interface Seen {
  hash?: string;
  text?: string;
  ids: string[];
  suppressed: boolean;
  at?: number;
}
interface Cursor {
  cursor: string | null;
  baseline: boolean;
  started: boolean;
  lastTurn?: string | undefined;
  head?: string | undefined;
}
export function whatsappText(
  text: string,
  t: Translator = translator(),
): string {
  const clean = text.replace(/::created-thread\{[^}]*\}/g, "").trim();
  return (
    clean || (/::created-thread\{/.test(text) ? t("listener.created") : "")
  );
}
export function terminalText(turn: Record<string, Wire.Json>, t: Translator = translator()): string {
  const error = turn['error'];
  const reason = error && typeof error === 'object' && !Array.isArray(error) && typeof error['message'] === 'string' ? error['message'].trim() : '';
  if (turn['status'] === 'failed' && reason) {
    if (/workspace routing discovery unauthorized|access token could not be refreshed|refresh token.*(?:expired|revoked|already.*used)|not authenticated|not logged in/i.test(reason))
      return t('listener.auth_required');
    return t('listener.failed', { reason: reason.slice(0, 600) });
  }
  return t('listener.terminal', { status: String(turn['status']) });
}
export function delegationSource(
  item: Record<string, Wire.Json>,
): string | null {
  if (!["userMessage", "steeringUserMessage"].includes(String(item["type"])))
    return null;
  const content = Array.isArray(item["content"]) ? item["content"] : [];
  for (const entry of content)
    if (entry && typeof entry === "object" && !Array.isArray(entry)) {
      const metadata = entry["codexDelegation"];
      if (
        metadata &&
        typeof metadata === "object" &&
        !Array.isArray(metadata) &&
        typeof metadata["sourceThreadId"] === "string"
      )
        return metadata["sourceThreadId"];
    }
  const texts = [
    item["text"],
    ...content.map((entry) =>
      entry && typeof entry === "object" && !Array.isArray(entry)
        ? entry["text"]
        : null,
    ),
  ];
  for (const text of texts)
    if (
      typeof text === "string" &&
      text.trim().startsWith("<codex_delegation>") &&
      text.trim().endsWith("</codex_delegation>")
    ) {
      const id = /<source_thread_id>\s*([^<]+?)\s*<\/source_thread_id>/.exec(
        text,
      )?.[1];
      if (id) return id.trim();
    }
  return null;
}
/** Independent of ChatWorker's queue. Native message identities, not turns, own delivery. */
export class WhatsAppListener {
  needsRecovery = false;
  private recovery: {
    bid: string;
    cursor: Cursor;
    next: string | null;
    head: string | undefined;
    turns: Array<Record<string, Wire.Json>>;
    bytes: number;
    busy: boolean;
  } | null = null;
  private sources = new Map<string, string | null>();
  private names = new Map<string, string>();
  private source(bid: string, turnId: string, item: Record<string, Wire.Json>): void {
    if (!["userMessage", "steeringUserMessage"].includes(String(item["type"])))
      return;
    const id = delegationSource(item);
    this.sources.set(turnId, id);
    if (typeof item['clientId'] === 'string' && this.bridge.main.store.isNoticeInput(item['clientId']))
      this.journal.set(`notice-turn:${bid}:${turnId}`, true);
    if (this.sources.size > 256)
      this.sources.delete(this.sources.keys().next().value!);
    if (id && !this.names.has(id)) {
      this.names.set(id, this.t("listener.task", { id: id.slice(0, 8) }));
      if (this.names.size > 256)
        this.names.delete(this.names.keys().next().value!);
      // Naming is cosmetic and must not hold response delivery.
      void this.native.owner
        .read(
          "thread/read",
          { threadId: id, includeTurns: false },
          undefined,
          true,
        )
        .then((value) => {
          if ("result" in value.reply) {
            const thread = (
              value.reply.result as { thread?: { name?: string } }
            ).thread;
            if (thread?.name) this.names.set(id, thread.name);
          }
        })
        .catch(() => {});
    }
  }
  private text(text: string, turnId: string): string {
    const source = this.sources.get(turnId),
      value = whatsappText(text, this.t);
    return source && value
      ? `[${this.names.get(source) ?? this.t("listener.task", { id: source.slice(0, 8) })}]\n${value}`
      : value;
  }
  constructor(
    readonly bridge: ChatBridge,
    readonly journal: WhatsAppJournal,
    readonly native: WhatsAppNative,
    readonly config: WhatsAppConfig,
    readonly t: Translator = translator(),
  ) {}
  async notification(
    event: Agent.Notification,
    commentary: boolean,
  ): Promise<{ busy: boolean | null } | null> {
    // Streaming deltas and unrelated native events need no Main/Object lookup.
    if (
      !["turn/started", "turn/completed", "item/completed"].includes(
        event.method,
      )
    )
      return null;
    const params = event.params as Record<string, Wire.Json>;
    const item = params["item"];
    if (
      event.method === "item/completed" &&
      (!item ||
        typeof item !== "object" ||
        Array.isArray(item) ||
        !["agentMessage", "userMessage", "steeringUserMessage"].includes(
          String(item["type"]),
        ))
    )
      return null;
    const { binding } = await this.native.bound(),
      bid = binding.pin.objectId;
    if (params["threadId"] !== binding.value.primary.nativeId) return null;
    if (event.method === "turn/started") return { busy: true };
    if (event.method === "turn/completed") return { busy: false };
    if (event.method !== "item/completed") return null;
    if (
      item &&
      typeof item === "object" &&
      !Array.isArray(item) &&
      typeof params["turnId"] === "string"
    )
      this.source(bid, params["turnId"], item);
    if (
      !item ||
      typeof item !== "object" ||
      Array.isArray(item) ||
      item["type"] !== "agentMessage"
    )
      return null;
    requireThat(
      typeof params["turnId"] === "string" &&
        typeof item["id"] === "string" &&
        typeof item["text"] === "string",
      "native_read_failed",
      "Completed native message identity missing.",
    );
    // Complete turn reads recover missing or out-of-order user-message events.
    if (!this.sources.has(params['turnId']) || this.journal.get(`notice-turn:${bid}:${params['turnId']}`)) return { busy: null };
    for (const sender of this.config.senders) {
      const key = `message:${bid}:${params["turnId"]}:${item["id"]}:${sender.channelId}`,
        existing = this.journal.get<Seen>(key);
      if (existing) {
        requireThat(
          (existing.hash ?? identity(existing.text)) === identity(item["text"]),
          "whatsapp_native_message_changed",
          "A completed native message changed.",
        );
        continue;
      }
      const suppressed = item["phase"] === "commentary" && !commentary;
      this.journal.transaction(() => {
        const ids = suppressed
          ? []
          : this.journal.enqueue(
              key,
              phone(sender.phoneNumber) + "@s.whatsapp.net",
              this.text(item["text"] as string, params["turnId"] as string),
            );
        this.journal.set(key, {
          hash: identity(item["text"]),
          ids,
          suppressed,
          at: Date.now(),
        } satisfies Seen);
      });
    }
    return { busy: null };
  }
  async tick(commentary: boolean): Promise<boolean> {
    const { binding } = await this.native.bound(),
      bid = binding.pin.objectId;
    const key = "listener:" + bid;
    let cursor = this.journal.get<Cursor>(key);
    if (!cursor) {
      // A fresh, explicitly created Main has no historical outputs to skip. Existing bindings
      // establish a read-only baseline once; this never sends earlier conversation history.
      cursor = {
        cursor: null,
        baseline: !this.journal.get<boolean>("listener-new:" + bid),
        started: false,
      };
      this.journal.set(key, cursor);
    }
    // A partial catch-up is disposable memory. Persist the watermark only after delivery intent
    // exists, so a process restart can never skip the newest pages of an interrupted catch-up.
    if (this.recovery?.bid !== bid)
      this.recovery = {
        bid,
        cursor,
        next: null,
        head: undefined,
        turns: [],
        bytes: 0,
        busy: false,
      };
    const recovery = this.recovery;
    cursor = recovery.cursor;
    this.needsRecovery = false;
    let done = false; const began = Date.now();
    for (let pageIndex = 0; pageIndex < 64 && !done; pageIndex++) {
      const page = await this.native.turnPage(binding.value.primary.nativeId, recovery.next);
      const turns = page["data"] as Array<Record<string, Wire.Json>>;
      requireThat(
        Array.isArray(turns),
        "native_read_failed",
        "Native turn page missing.",
      );
      // Tool payloads are needed by native evidence consumers, not message delivery.
      for (const turn of turns) if (Array.isArray(turn['items'])) turn['items'] = turn['items'].map(raw => {
        const item = raw as Record<string, Wire.Json>;
        return ['userMessage', 'steeringUserMessage', 'agentMessage'].includes(String(item['type'])) ? item : { id: item['id']!, type: item['type']! };
      });
      recovery.bytes += Buffer.byteLength(JSON.stringify(turns));
      if (recovery.bytes > 32 * 1024 * 1024) {
        this.recovery = null;
        requireThat(
          false,
          "native_history_gap",
          "History recovery exceeds the bounded buffer; live interactions remain available.",
        );
      }
      if (recovery.head === undefined)
        recovery.head = turns[0]?.["id"] as string | undefined;
      for (const turn of turns) {
        recovery.turns.push(turn);
        recovery.busy ||= turn["status"] === "inProgress";
        if (turn["id"] === cursor.lastTurn) {
          done = true;
          break;
        }
      }
      const next = page["nextCursor"];
      requireThat(
        next === null || (typeof next === "string" && next !== recovery.next),
        "native_read_failed",
        "Native cursor did not progress.",
      );
      recovery.next = next;
      done ||= cursor.baseline || next === null;
      if (Date.now() - began >= 12000) break;
    }
    if (!done) {
      this.needsRecovery = true;
      return recovery.busy;
    }
    const busy = recovery.busy,
      head = recovery.head;
    for (const turn of [...recovery.turns].reverse()) {
      const turnId = String(turn["id"]),
        turnBusy = turn["status"] === "inProgress";
      requireThat(
        turn["itemsView"] === "full" && Array.isArray(turn["items"]),
        "native_read_failed",
        "A complete native turn is required.",
      );
      const items = turn["items"] as Array<Record<string, Wire.Json>>,
        all: string[] = [];
      let finalCount = 0;
      for (let index = 0; index < items.length; index++) {
        const item = items[index]!;
        this.source(bid, turnId, item);
        if (item["type"] !== "agentMessage") continue;
        if (this.journal.get(`notice-turn:${bid}:${turnId}`)) { finalCount++; continue; }
        const isCommentary = item["phase"] === "commentary";
        if (!isCommentary) finalCount++;
        // Last streaming item is not known complete; a following item or terminal turn fences it.
        if (turnBusy && index === items.length - 1) continue;
        requireThat(
          typeof item["id"] === "string" && typeof item["text"] === "string",
          "native_read_failed",
          "Native message identity missing.",
        );
        for (const sender of this.config.senders) {
          const messageKey = `message:${bid}:${turnId}:${item["id"]}:${sender.channelId}`,
            existing = this.journal.get<Seen>(messageKey);
          if (existing) {
            requireThat(
              (existing.hash ?? identity(existing.text)) ===
                identity(item["text"]),
              "whatsapp_native_message_changed",
              "A delivered native message changed.",
            );
            all.push(...existing.ids);
            continue;
          }
          const suppressed = cursor.baseline || (isCommentary && !commentary),
            jid = phone(sender.phoneNumber) + "@s.whatsapp.net";
          this.journal.transaction(() => {
            const ids = suppressed
              ? []
              : this.journal.enqueue(
                  messageKey,
                  jid,
                  this.text(item["text"] as string, turnId),
                );
            all.push(...ids);
            this.journal.set(messageKey, {
              hash: identity(item["text"]),
              ids,
              suppressed,
              at: Date.now(),
            } satisfies Seen);
          });
        }
      }
      if (!turnBusy && !finalCount && !cursor.baseline) {
        for (const sender of this.config.senders) {
          const terminalKey = `terminal:${bid}:${turnId}:${sender.channelId}`;
          all.push(
            ...this.journal.enqueue(
              terminalKey,
              phone(sender.phoneNumber) + "@s.whatsapp.net",
              this.journal.noticeText(terminalKey) ?? terminalText(turn, this.t),
            ),
          );
        }
      }
      this.journal.set(
        `turn-delivered:${bid}:${turnId}`,
        !turnBusy && (this.journal.sent(all) ? true : all),
      );
    }
    this.journal.set(key, {
      cursor: null,
      baseline: false,
      started: true,
      lastTurn: head,
      head,
    } satisfies Cursor);
    this.recovery = null;
    return busy;
  }
  async outbox(): Promise<void> {
    for (const sender of this.config.senders) {
      const channel: Chat.Channel = {
          adapter: "whatsapp",
          accountId: this.config.accountId,
          channelId: sender.channelId,
        },
        expectedBridge = this.bridge.main.admission.expected(
          sender.principalId,
        );
      const partition = this.bridge.main.admission.definitionHash + ':' + sender.channelId;
      const key = "offer:" + partition;
      let pending = this.journal.get<{
        request: Chat.ReceiveRequest;
        offer?: Chat.OfferView;
        replies?: Chat.ReplyView[];
        ack?: Chat.AcknowledgeRequest;
      }>(key);
      if (
        pending?.offer &&
        !(await this.bridge.main.operations.find(
          sender.principalId,
          pending.request.operationId,
        ))
      ) {
        // Delivery may outlive the offer's retention window. Reoffer from the saved cursor;
        // outgoing message identities and completed native delivery markers remain unchanged.
        this.journal.set(key, null);
        continue;
      }
      if (!pending) {
        const afterSequence =
          this.journal.get<number>("reply-cursor:" + partition) ?? 0;
        const history = await this.bridge.outbox.history(sender.principalId, {
          expectedBridge,
          channel,
          afterSequence,
          limit: 8,
        });
        if (!history.entries.some((e) => "origin" in e.data)) {
          if (history.throughSequence > afterSequence)
            this.journal.set(
              "reply-cursor:" + partition,
              history.throughSequence,
            );
          continue;
        }
        pending = {
          request: {
            action: "receive",
            operationId: await scopedOperationId(
              this.bridge.main.store.client,
              ["whatsapp-receive", partition, afterSequence],
            ),
            expectedBridge,
            channel,
            afterSequence,
            limit: 8,
          },
        };
        this.journal.set(key, pending);
      }
      if (!pending.offer) {
        const result = await this.bridge.outbox.receive(
          sender.principalId,
          pending.request,
        );
        if (
          result.phase !== "succeeded" ||
          result.outcome?.action !== "receive"
        )
          continue;
        pending.offer = result.outcome.offer;
        pending.replies = result.outcome.replies;
        this.journal.set(key, pending);
      }
      let complete = true;
      for (const reply of pending.replies!) {
        const canonicalNotice = reply.data.origin.kind === 'native' && this.bridge.main.store.isNoticeInput(mutation(reply.data.origin.inputId, 'native-user-message'));
        if (canonicalNotice) {
          const saved = await this.bridge.outbox.reply(reply.object.objectId, reply.data.channel);
          if (saved.value.state !== 'confirmed') complete = false;
        } else if (reply.data.origin.kind === "native") {
          const result = await this.bridge.main.store.read(
            "chat-bridge/result",
            reply.data.origin.result,
            reply.data.origin.inputId,
          );
          const deliveryKey = `turn-delivered:${reply.data.origin.binding.objectId}:${result.value.turnId}`;
          const delivered = this.journal.get<boolean | string[]>(deliveryKey);
          // Transport acknowledgements can arrive after history has moved to another turn.
          // Keep the exact output IDs so delivery does not depend on rereading old turns.
          if (Array.isArray(delivered) && this.journal.sent(delivered))
            this.journal.set(deliveryKey, true);
          else if (delivered !== true) complete = false;
        } else {
          const ids = this.journal.enqueue(
            "notice:" + reply.object.objectId,
            phone(sender.phoneNumber) + "@s.whatsapp.net",
            reply.data.text,
          );
          if (!this.journal.sent(ids)) complete = false;
        }
      }
      if (!complete) continue;
      if (pending.replies!.length) {
        pending.ack ??= {
          action: "acknowledge",
          operationId: await scopedOperationId(this.bridge.main.store.client, [
            "whatsapp-acknowledge",
            pending.offer.object.objectId,
          ]),
          expectedBridge,
          offerId: pending.offer.object.objectId,
          replyIds: pending.replies!.map((r) => r.object.objectId),
        };
        this.journal.set(key, pending);
        const result = await this.bridge.outbox.acknowledge(
          sender.principalId,
          pending.ack,
        );
        if (result.phase !== "succeeded") continue;
      }
      this.journal.transaction(() => {
        this.journal.set(
          "reply-cursor:" + partition,
          pending!.offer!.data.throughSequence,
        );
        this.journal.set(key, null);
      });
    }
  }
  private noticeCursor: string | undefined;
  /** Deliver exact pending service replies across native/configuration changes. */
  async notices(): Promise<void> {
    const page = await this.bridge.outbox.pendingNotices(this.noticeCursor);
    const sender = this.config.senders[0]!;
    for (const reply of page.replies) {
      // Reuse any existing transport receipt for this exact reply, including one queued
      // before the current configuration. Never synthesize a new logical message on recovery.
      const parts = await this.bridge.outbox.noticeParts(reply);
      const ids = parts.flatMap(part => {
        const prior = 'mirrored-notice:' + sender.channelId + ':' + part.pin.objectId;
        const key = this.journal.hasOutput(prior) ? prior : 'notice:' + part.pin.objectId;
        return this.journal.enqueue(key, phone(sender.phoneNumber) + '@s.whatsapp.net', part.value.text);
      });
      if (this.journal.sent(ids)) await this.bridge.outbox.confirmNotice(parts);
    }
    this.noticeCursor = page.nextCursor ?? undefined;
  }
}
