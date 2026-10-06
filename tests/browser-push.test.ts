import test from "node:test";
import assert from "node:assert/strict";
import { createECDH, randomBytes, randomUUID } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { runInNewContext } from "node:vm";
import { HiveStore } from "../services/hive/src/store.js";
import { HiveServer } from "../services/hive/src/server.js";
import {
  BrowserPushStore,
  pushSubscription,
} from "../services/hive/src/browser-push-store.js";
import type { PushBatch } from "../services/hive/src/browser-push-store.js";
import { browserAppWorker } from "../services/hive/src/browser-app.js";
import { digest } from "../packages/contracts/src/canonical.js";
import { browserNoticeTopic } from "../packages/contracts/src/browser-notice.js";
import type { BrowserNotice } from "../packages/contracts/src/browser-notice.js";
import { nativeBrowserNotice } from "../services/agent-manager/src/notifications.js";
import { Events } from "../services/hive/src/events.js";
import { ServiceClient } from "../packages/sdk/src/service.js";
import { publishBrowserNotice } from "../packages/sdk/src/browser-notice.js";

const credential = digest("test-push-credential"),
  base = new URL("https://example.test/ivy/");
function subscription(id = "test") {
  const key = createECDH("prime256v1");
  key.generateKeys();
  return {
    endpoint: "https://fcm.googleapis.com/fcm/send/" + id,
    keys: {
      auth: randomBytes(16).toString("base64url"),
      p256dh: key.getPublicKey().toString("base64url"),
    },
  };
}
const context = {
  principalId: "client",
  credentialDigest: credential,
  sessionDigest: "session-one",
};
function initialize(store: HiveStore) {
  store.configureCredentials([
    { principalId: context.principalId, digest: credential },
  ]);
  store.run(
    "INSERT OR IGNORE INTO browser_sessions VALUES (?,?,?)",
    context.sessionDigest,
    credential,
    new Date().toISOString(),
  );
  new Events(store).registerTopic(
    browserNoticeTopic("agent"),
    "service:agent-manager",
  );
  return new BrowserPushStore(store, base);
}
const notice = (method = "turn/completed") =>
  nativeBrowserNotice("agent-one", {
    method,
    params: {
      threadId: "thread & one",
      turn: { id: "turn-one", status: "completed" },
    },
  });
function publish(store: HiveStore, value: BrowserNotice | null) {
  if (value)
    store.run(
      "INSERT INTO events(topic,topic_version,source,occurred_at,mutation_id,payload) VALUES (?,?,?,?,?,?)",
      "agent.browser-notification",
      "1.0.0",
      "service:agent-one",
      new Date().toISOString(),
      randomUUID(),
      JSON.stringify(value),
    );
}

test("push subscriptions reject non-browser endpoints and malformed encryption keys", () => {
  const valid = subscription();
  for (const endpoint of [
    valid.endpoint,
    "https://fcm.googleapis.com/wp/token",
    "https://fcm.googleapis.com/preprod/wp/token",
    "https://jmt17.google.com/fcm/send/token",
  ])
    assert.deepEqual(pushSubscription({ ...valid, endpoint }), {
      ...valid,
      endpoint,
    });
  assert.equal(
    pushSubscription({
      ...valid,
      endpoint: "https://regional.push.apple.com/token",
    }).endpoint,
    "https://regional.push.apple.com/token",
  );
  for (const endpoint of [
    "http://fcm.googleapis.com/x",
    "https://127.0.0.1/x",
    "https://fcm.googleapis.com.evil.test/x",
    "https://jmt17.google.com.evil.test/x",
    "https://other.google.com/fcm/send/token",
    "https://user:secret@fcm.googleapis.com/x",
    "https://fcm.googleapis.com:8443/x",
    "broken",
  ])
    assert.throws(() => pushSubscription({ ...valid, endpoint }));
  assert.throws(() =>
    pushSubscription({ ...valid, keys: { ...valid.keys, auth: "x" } }),
  );
  assert.throws(() =>
    pushSubscription({
      ...valid,
      keys: { ...valid.keys, p256dh: Buffer.alloc(65).toString("base64url") },
    }),
  );
});

test("keys, subscriptions and queued pushes survive restart; logout and revocation end delivery", (t) => {
  const root = mkdtempSync(join(tmpdir(), "ivy-push-"));
  let store = new HiveStore(join(root, "hive.sqlite"));
  t.after(() => {
    store.close();
    rmSync(root, { recursive: true, force: true });
  });
  let push = initialize(store);
  const keys = push.keys(),
    sub = subscription();
  assert.throws(
    () =>
      push.request(
        { principalId: "client", credentialDigest: credential },
        { action: "subscribe", subscription: sub },
      ),
    /browser session/,
  );
  push.request(context, { action: "subscribe", subscription: sub });
  push.request(context, { action: "test", endpoint: sub.endpoint });
  const inFlight = push.next().deliveries[0]!;
  push.request(context, { action: "unsubscribe", endpoint: sub.endpoint });
  push.request(context, { action: "subscribe", subscription: sub });
  push.request(context, { action: "test", endpoint: sub.endpoint });
  const replacement = push.next().deliveries[0]!;
  push.complete(inFlight.id, 410);
  assert.notEqual(
    replacement.id,
    inFlight.id,
    "late responses cannot remove a replacement subscription",
  );
  assert.equal(push.next().deliveries.length, 1);
  push.complete(replacement.id, 201);
  publish(store, notice("item/agentMessage/delta"));
  assert.equal(push.next().deliveries.length, 0);
  publish(store, notice());
  publish(store, notice());
  store.close();
  store = new HiveStore(join(root, "hive.sqlite"));
  push = initialize(store);
  assert.deepEqual(push.keys(), keys);
  const pending = push.next().deliveries;
  assert.equal(pending.length, 1);
  assert.equal(new URL(pending[0]!.notice.url).pathname, "/ivy/ui/agent-ui/");
  assert.match(pending[0]!.notice.url, /thread\+%26\+one/);
  push.complete(pending[0]!.id, 503);
  assert.equal(push.next().deliveries.length, 0, "failed delivery backs off");
  assert.match(
    String(
      (
        push.request(context, { action: "status", endpoint: sub.endpoint }) as {
          lastError: string;
        }
      ).lastError,
    ),
    /failed/,
  );
  store.run("UPDATE browser_push_outbox SET next_attempt=0");
  push.complete(push.next().deliveries[0]!.id, 410);
  assert.equal(
    (
      push.request(context, { action: "status", endpoint: sub.endpoint }) as {
        subscribed: boolean;
      }
    ).subscribed,
    false,
  );
  push.request(context, { action: "subscribe", subscription: sub });
  publish(store, { ...notice()!, tag: "logout" });
  store.run(
    "DELETE FROM browser_sessions WHERE digest=?",
    context.sessionDigest,
  );
  assert.equal(push.next().deliveries.length, 0);
  store.run(
    "INSERT INTO browser_sessions VALUES (?,?,?)",
    context.sessionDigest,
    credential,
    new Date().toISOString(),
  );
  push.request(context, { action: "subscribe", subscription: sub });
  publish(store, { ...notice()!, tag: "revoked" });
  store.configureCredentials([]);
  assert.equal(push.next().deliveries.length, 0);
});

test("HTTP push actions require the signed-in browser and exact Origin; settings are session-bound", async (t) => {
  const sent: string[] = [];
  let service: ServiceClient | undefined;
  const server = new HiveServer({
    filename: ":memory:",
    publicBaseUrl: base.href,
    version: "test",
    buildId: digest("test"),
    credentials: [{ principalId: "client", digest: credential }],
    listenPort: 0,
    sendBrowserPush: async (_sub, payload) => {
      sent.push(String(payload));
      return { statusCode: 201, headers: {}, body: "" };
    },
  });
  t.after(async () => {
    await service?.stop();
    await server.close();
  });
  const address = await server.start(),
    local = `http://127.0.0.1:${address.port}/ivy/`;
  const manifest = await fetch(local + "app/manifest.webmanifest");
  assert.equal(manifest.status, 200);
  const app = (await manifest.json()) as {
    scope: string;
    start_url: string;
    id: string;
  };
  assert.equal(app.id, "/ivy/");
  assert.equal(app.scope, "/ivy/");
  assert.equal(app.start_url, "/ivy/");
  const worker = await fetch(local + "app-worker.js");
  assert.equal(worker.headers.get("service-worker-allowed"), "/ivy/");
  assert.equal(worker.headers.get("cache-control"), "no-store");
  const login = async () => {
    const response = await fetch(local + "login", {
      method: "POST",
      redirect: "manual",
      headers: {
        Origin: base.origin,
        "Content-Type": "application/x-www-form-urlencoded",
      },
      body: new URLSearchParams({ token: "test-push-credential" }),
    });
    return response.headers.get("set-cookie")!.split(";")[0]!;
  };
  const cookie = await login(),
    other = await login(),
    sub = subscription();
  const call = (value: unknown, identity = cookie, origin = base.origin) =>
    fetch(local + "api/browser-push", {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        Cookie: identity,
        Origin: origin,
      },
      body: JSON.stringify(value),
    });
  assert.equal(
    (await call({ action: "subscribe", subscription: sub }, "")).status,
    401,
  );
  assert.equal(
    (
      await call(
        { action: "subscribe", subscription: sub },
        cookie,
        "https://elsewhere.test",
      )
    ).status,
    401,
  );
  assert.equal(
    (await call({ action: "subscribe", subscription: sub })).status,
    200,
  );
  await call({ action: "unsubscribe", endpoint: sub.endpoint }, other);
  const status = (await (
    await call({ action: "status", endpoint: sub.endpoint })
  ).json()) as { subscribed: boolean; publicKey: string; privateKey?: string };
  assert.equal(status.subscribed, true);
  assert.ok(status.publicKey);
  assert.equal(status.privateKey, undefined);
  assert.equal(
    (await call({ action: "test", endpoint: sub.endpoint }, other)).status,
    404,
  );
  assert.equal(
    (await call({ action: "test", endpoint: sub.endpoint })).status,
    200,
  );
  for (let i = 0; i < 100 && !sent.length; i++)
    await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(sent.length, 1);
  assert.match(sent[0]!, /Browser notifications are working/);
  let ready!: () => void;
  const connected = new Promise<void>((resolve) => {
    ready = resolve;
  });
  service = new ServiceClient({
    publicBaseUrl: local,
    credential: () => "test-push-credential",
    identity: {
      serviceNodeId: "push-fixture",
      serviceName: "push-fixture",
      hostId: "fixture",
      version: "1.0.0",
      buildId: digest("push-fixture"),
      hiveProtocol: 1,
    },
    registry: () => ({
      namespaces: [
        {
          namespace: "fixture",
          description: "Push fixture",
          guideMarkdown: "",
          tools: [],
          inventoryKinds: [],
          topics: [browserNoticeTopic("fixture")],
        },
      ],
      contracts: [],
      requiredContracts: [],
    }),
    handlers: {},
    reconcile: async () => {},
    onState: (value) => {
      if (value.status === "ready") ready();
    },
  });
  service.start();
  let timer: NodeJS.Timeout | undefined;
  try {
    await Promise.race([
      connected,
      new Promise((_, reject) => {
        timer = setTimeout(
          () => reject(new Error("Push fixture did not connect")),
          10000,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
  await publishBrowserNotice(service.connection, "fixture", notice()!);
  for (let i = 0; i < 100 && sent.length < 2; i++)
    await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(
    sent.length,
    2,
    "a service event wakes server push without any open browser page",
  );
  assert.match(sent[1]!, /ui\/agent-ui/);
  await fetch(local + "logout", {
    method: "POST",
    redirect: "manual",
    headers: { Cookie: cookie, Origin: base.origin },
  });
  assert.equal(
    (await call({ action: "test", endpoint: sub.endpoint })).status,
    401,
  );
  const queue = await server.worker.request<PushBatch>({
    action: "browserPush.next",
  });
  assert.equal(queue.deliveries.length, 0);
});

test("push worker shows notices without a page and opens only its own installation", async () => {
  const handlers = new Map<string, (event: unknown) => void>(),
    shown: unknown[] = [],
    opened: string[] = [],
    navigated: string[] = [];
  let windows: {
    url: string;
    navigate(url: string): Promise<void>;
    focus(): Promise<void>;
  }[] = [];
  let focused = false;
  runInNewContext(browserAppWorker, {
    URL,
    self: {
      registration: {
        scope: base.href,
        showNotification: async (...args: unknown[]) => {
          shown.push(args);
        },
      },
      addEventListener: (name: string, handler: (event: unknown) => void) =>
        handlers.set(name, handler),
      clients: {
        matchAll: async () => windows,
        openWindow: async (url: string) => opened.push(url),
      },
    },
  });
  const dispatch = async (name: string, value: Record<string, unknown>) => {
    let work: Promise<unknown> | undefined;
    handlers.get(name)!({
      ...value,
      waitUntil: (promise: Promise<unknown>) => {
        work = promise;
      },
    });
    await work;
  };
  await dispatch("push", {
    data: {
      json: () => ({
        title: "Result",
        body: "Finished",
        url: base.href + "ui/agent-ui/#/task?id=one",
      }),
    },
  });
  assert.equal(shown.length, 1);
  for (const url of [
    "https://elsewhere.test/",
    "https://example.test/ivy-other/",
    "javascript:alert(1)",
  ])
    await dispatch("notificationclick", {
      notification: { close() {}, data: { url } },
    });
  assert.deepEqual(opened, [base.href, base.href, base.href]);
  windows = [
    {
      url: base.href,
      navigate: async (url) => {
        navigated.push(url);
      },
      focus: async () => {
        focused = true;
      },
    },
  ];
  const target = base.href + "ui/agent-ui/#/task?id=one";
  await dispatch("notificationclick", {
    notification: { close() {}, data: { url: target } },
  });
  assert.deepEqual(navigated, [target]);
  assert.equal(focused, true);
  assert.equal(opened.length, 3, "existing Ivy window is reused");
});
