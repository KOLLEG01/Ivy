import webPush from "web-push";
import { ECDH } from "node:crypto";
import { requireThat } from "../../../packages/contracts/src/errors.js";
import { browserNoticeSchema } from "../../../packages/contracts/src/browser-notice.js";
import type { BrowserNotice as ServiceNotice } from "../../../packages/contracts/src/browser-notice.js";
import { digest } from "../../../packages/contracts/src/canonical.js";
import type { HiveStore, AuthenticatedContext } from "./store.js";

export interface BrowserNotice {
  title: string;
  body: string;
  tag: string;
  url: string;
}
export interface PushDelivery {
  id: number;
  subscription: webPush.PushSubscription;
  notice: BrowserNotice;
  attempts: number;
}
export interface PushBatch {
  deliveries: PushDelivery[];
  hasMore: boolean;
}
export type BrowserPushRequest =
  | { action: "status"; endpoint?: string }
  | { action: "subscribe"; subscription: unknown }
  | { action: "unsubscribe"; endpoint: string }
  | { action: "test"; endpoint: string };

export function pushSubscription(input: unknown): webPush.PushSubscription {
  const value = input as webPush.PushSubscription | null;
  requireThat(
    value &&
      typeof value.endpoint === "string" &&
      value.endpoint.length <= 4096,
    "invalid_arguments",
    "A push subscription is required.",
  );
  let url: URL;
  try {
    url = new URL(value.endpoint);
  } catch {
    requireThat(false, "invalid_arguments", "Invalid push endpoint.");
  }
  // These are browser-owned push services. Restrict outbound requests rather than
  // allowing a subscription to make Hive contact arbitrary internal HTTP servers.
  // Chromium's non-stable channels also use Google's jmt17 push service.
  requireThat(
    url.protocol === "https:" &&
      !url.username &&
      !url.password &&
      !url.port &&
      !url.hash &&
      (url.hostname === "fcm.googleapis.com" ||
        url.hostname === "jmt17.google.com" ||
        url.hostname === "updates.push.services.mozilla.com" ||
        url.hostname.endsWith(".push.services.mozilla.com") ||
        url.hostname.endsWith(".push.apple.com") ||
        url.hostname.endsWith(".notify.windows.com")),
    "invalid_arguments",
    "Unsupported browser push service.",
  );
  const key = (name: "auth" | "p256dh", length: number) => {
    const text = value.keys?.[name];
    requireThat(
      typeof text === "string" &&
        /^[A-Za-z0-9_-]+$/.test(text) &&
        Buffer.from(text, "base64url").length === length,
      "invalid_arguments",
      "Invalid push encryption keys.",
    );
    return text;
  };
  const auth = key("auth", 16),
    p256dh = key("p256dh", 65);
  try {
    ECDH.convertKey(Buffer.from(p256dh, "base64url"), "prime256v1");
  } catch {
    requireThat(false, "invalid_arguments", "Invalid push encryption key.");
  }
  return { endpoint: url.href, keys: { auth, p256dh } };
}

/** Storage stays in Hive's worker; network delivery never holds its request chain. */
export class BrowserPushStore {
  constructor(
    readonly store: HiveStore,
    readonly base: URL,
  ) {
    store.db.exec(`CREATE TABLE IF NOT EXISTS browser_push (
      endpoint TEXT PRIMARY KEY, session_digest TEXT NOT NULL REFERENCES browser_sessions(digest) ON DELETE CASCADE,
      subscription_json TEXT NOT NULL, last_error TEXT, created_sequence INTEGER NOT NULL
    ) STRICT;
    CREATE TABLE IF NOT EXISTS browser_push_outbox (
      id INTEGER PRIMARY KEY AUTOINCREMENT, endpoint TEXT NOT NULL REFERENCES browser_push(endpoint) ON DELETE CASCADE,
      tag TEXT NOT NULL, notice_json TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
      next_attempt INTEGER NOT NULL DEFAULT 0, UNIQUE(endpoint,tag)
    ) STRICT;
    CREATE TABLE IF NOT EXISTS browser_push_receipts (tag TEXT PRIMARY KEY, created_at INTEGER NOT NULL) STRICT;`);
    if (!store.metadata("browser_push_cursor"))
      store.setMetadata("browser_push_cursor", String(this.head()));
  }
  keys(): webPush.VapidKeys {
    let value = this.store.metadata("browser_push_keys");
    if (!value) {
      value = JSON.stringify(webPush.generateVAPIDKeys());
      this.store.setMetadata("browser_push_keys", value);
    }
    return JSON.parse(value) as webPush.VapidKeys;
  }
  request(context: AuthenticatedContext, request: BrowserPushRequest): unknown {
    this.store.authenticate(context);
    requireThat(
      context.sessionDigest,
      "forbidden",
      "Push settings require a browser session.",
    );
    const session = context.sessionDigest;
    if (request.action === "status") {
      const row = request.endpoint
        ? this.store.get(
            "SELECT last_error FROM browser_push WHERE endpoint=? AND session_digest=?",
            request.endpoint,
            session,
          )
        : undefined;
      return {
        publicKey: this.keys().publicKey,
        subscribed: !!row,
        lastError: row?.["last_error"] ?? null,
      };
    }
    if (request.action === "subscribe") {
      const subscription = pushSubscription(request.subscription),
        encoded = JSON.stringify(subscription);
      const existing = this.store.get(
        "SELECT subscription_json FROM browser_push WHERE endpoint=?",
        subscription.endpoint,
      );
      requireThat(
        !existing || existing["subscription_json"] === encoded,
        "forbidden",
        "This endpoint already has different encryption keys.",
      );
      requireThat(
        existing ||
          Number(
            this.store.get("SELECT COUNT(*) AS n FROM browser_push")?.["n"],
          ) < 1024,
        "limit_exceeded",
        "Push subscription capacity reached.",
      );
      requireThat(
        existing ||
          Number(
            this.store.get(
              "SELECT COUNT(*) AS n FROM browser_push WHERE session_digest=?",
              session,
            )?.["n"],
          ) < 16,
        "limit_exceeded",
        "Browser push subscription capacity reached.",
      );
      // A new login can rebind the same browser only with its complete secret subscription.
      this.store.transaction(() => {
        this.store.run(
          "DELETE FROM browser_push WHERE endpoint=?",
          subscription.endpoint,
        );
        this.store.run(
          "INSERT INTO browser_push VALUES (?,?,?,NULL,?)",
          subscription.endpoint,
          session,
          encoded,
          this.head(),
        );
      });
      return { subscribed: true };
    }
    requireThat(
      typeof request.endpoint === "string" && request.endpoint.length <= 4096,
      "invalid_arguments",
      "A push endpoint is required.",
    );
    if (request.action === "unsubscribe") {
      this.store.run(
        "DELETE FROM browser_push WHERE endpoint=? AND session_digest=?",
        request.endpoint,
        session,
      );
      return { subscribed: false };
    }
    requireThat(
      request.action === "test",
      "invalid_arguments",
      "Unknown push action.",
    );
    requireThat(
      this.store.get(
        "SELECT 1 FROM browser_push WHERE endpoint=? AND session_digest=?",
        request.endpoint,
        session,
      ),
      "not_found",
      "Enable notifications first.",
    );
    this.enqueue(
      {
        title: "Ivy",
        body: "Browser notifications are working.",
        tag: "ivy-test",
        url: this.root(),
      },
      request.endpoint,
    );
    return { queued: true };
  }
  private root() {
    return this.base.href.replace(/\/?$/, "/");
  }
  private link(uiId: string, hash: string): string {
    return new URL("ui/" + uiId + "/" + hash, this.root()).href;
  }
  private head(): number {
    return Number(
      this.store.get("SELECT MAX(sequence) AS n FROM events")?.["n"] ?? 0,
    );
  }
  private enqueue(notice: BrowserNotice, endpoint?: string, sequence?: number) {
    const rows = this.store.all(
      `SELECT p.endpoint FROM browser_push p JOIN browser_sessions s ON s.digest=p.session_digest
      JOIN credentials c ON c.digest=s.credential_digest WHERE c.revoked=0${endpoint ? " AND p.endpoint=?" : ""}${sequence === undefined ? "" : " AND p.created_sequence<?"}`,
      ...(endpoint ? [endpoint] : []),
      ...(sequence === undefined ? [] : [sequence]),
    );
    for (const row of rows)
      this.store.run(
        "INSERT OR IGNORE INTO browser_push_outbox(endpoint,tag,notice_json) VALUES (?,?,?)",
        row["endpoint"]!,
        notice.tag,
        JSON.stringify({
          ...notice,
          title: notice.title.slice(0, 100),
          body: notice.body.slice(0, 300),
        }),
      );
    this.store.run(
      "DELETE FROM browser_push_outbox WHERE id IN (SELECT id FROM browser_push_outbox ORDER BY id DESC LIMIT -1 OFFSET 2048)",
    );
  }
  private eventNotice(
    payload: unknown,
    source: string,
    sequence: number,
  ): void {
    try {
      this.store.validators.validate(browserNoticeSchema, payload);
    } catch {
      return;
    } // A different/future event shape cannot block the delivery queue.
    const notice = payload as ServiceNotice,
      tag = digest(source + "\0" + notice.tag);
    if (this.store.get("SELECT 1 FROM browser_push_receipts WHERE tag=?", tag))
      return;
    this.enqueue(
      {
        title: notice.title,
        body: notice.body,
        tag,
        url: this.link(notice.target.uiId, notice.target.fragment),
      },
      undefined,
      sequence,
    );
    this.store.run(
      "INSERT INTO browser_push_receipts VALUES (?,?)",
      tag,
      Date.now(),
    );
  }
  next(): PushBatch {
    let hasMore = false;
    this.store.transaction(() => {
      // Revocation invalidates push delivery even while no browser is connected.
      this.store.run(`DELETE FROM browser_push WHERE session_digest NOT IN
        (SELECT s.digest FROM browser_sessions s JOIN credentials c ON c.digest=s.credential_digest WHERE c.revoked=0)`);
      const cursor = Number(this.store.metadata("browser_push_cursor"));
      const rows = this.store.all(
        "SELECT sequence,topic,topic_version,source,payload FROM events WHERE sequence>? ORDER BY sequence LIMIT 100",
        cursor,
      );
      if (this.store.get("SELECT 1 FROM browser_push LIMIT 1"))
        for (const row of rows) {
          if (
            !String(row["topic"]).endsWith(".browser-notification") ||
            row["topic_version"] !== "1.0.0" ||
            !String(row["source"]).startsWith("service:")
          )
            continue;
          this.eventNotice(
            JSON.parse(String(row["payload"])),
            String(row["source"]),
            Number(row["sequence"]),
          );
        }
      if (rows.length)
        this.store.setMetadata(
          "browser_push_cursor",
          String(rows.at(-1)!["sequence"]),
        );
      hasMore = rows.length === 100;
      this.store.run(
        "DELETE FROM browser_push_receipts WHERE created_at<?",
        Date.now() - 7 * 86400_000,
      );
      this.store.run(
        "DELETE FROM browser_push_receipts WHERE tag IN (SELECT tag FROM browser_push_receipts ORDER BY created_at DESC LIMIT -1 OFFSET 10000)",
      );
    });
    const deliveries = this.store
      .all(
        `SELECT q.*,p.subscription_json FROM browser_push_outbox q JOIN browser_push p ON p.endpoint=q.endpoint
      WHERE q.next_attempt<=? ORDER BY q.id LIMIT 4`,
        Date.now(),
      )
      .map((row) => ({
        id: Number(row["id"]),
        attempts: Number(row["attempts"]),
        subscription: JSON.parse(String(row["subscription_json"])),
        notice: JSON.parse(String(row["notice_json"])),
      }));
    return { deliveries, hasMore };
  }
  complete(id: number, status: number): void {
    const row = this.store.get(
      "SELECT endpoint,attempts FROM browser_push_outbox WHERE id=?",
      id,
    );
    if (!row) return;
    if (status === 404 || status === 410) {
      this.store.run(
        "DELETE FROM browser_push WHERE endpoint=?",
        row["endpoint"]!,
      );
      return;
    }
    const ok = status >= 200 && status < 300,
      attempts = Number(row["attempts"]) + 1;
    this.store.run(
      "UPDATE browser_push SET last_error=? WHERE endpoint=?",
      ok
        ? null
        : "Push delivery failed. Try a test notification or enable notifications again.",
      row["endpoint"]!,
    );
    if (ok || attempts >= 5)
      this.store.run("DELETE FROM browser_push_outbox WHERE id=?", id);
    else
      this.store.run(
        "UPDATE browser_push_outbox SET attempts=?,next_attempt=? WHERE id=?",
        attempts,
        Date.now() + 30_000 * 2 ** (attempts - 1),
        id,
      );
  }
}
