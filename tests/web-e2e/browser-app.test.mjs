import test from "node:test";
import assert from "node:assert/strict";
import { createECDH, randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { runInNewContext } from "node:vm";
import { expect } from "@playwright/test";
import { fixture, fixtureToken } from "./fixtures/fixture.mjs";
import { startBrowserPushHive } from "../fixtures/native-browser-push.mjs";
import { publishUi } from "../../dist/packages/cli/src/publish-ui.js";
import { newOperationId } from "../../dist/packages/sdk/src/client.js";
import { ServiceClient } from "../../dist/packages/sdk/src/service.js";
import {
  browserNoticeTopic,
  publishBrowserNotice,
} from "../../dist/packages/sdk/src/browser-notice.js";
import { digest } from "../../dist/packages/contracts/src/canonical.js";

const installBanner = (page) =>
  page.getByRole("region", { name: "Install Ivy", exact: true });

async function workerScope(source, base) {
  const handlers = new Map(),
    shown = [],
    opened = [],
    navigated = [];
  let windows = [],
    focused = false;
  runInNewContext(source, {
    URL,
    self: {
      registration: {
        scope: base,
        showNotification: async (...args) => {
          shown.push(args);
        },
      },
      addEventListener: (name, handler) => handlers.set(name, handler),
      clients: {
        matchAll: async () => windows,
        openWindow: async (url) => opened.push(url),
      },
    },
  });
  const dispatch = async (name, value) => {
    let work;
    handlers.get(name)({
      ...value,
      waitUntil: (promise) => {
        work = promise;
      },
    });
    await work;
  };
  const target = base + "ui/agent-ui/#/task?id=one";
  await dispatch("push", {
    data: { json: () => ({ title: "Result", body: "Finished", url: target }) },
  });
  assert.equal(shown.length, 1);
  for (const url of [
    "https://elsewhere.test/",
    new URL("/ivy-other/", base).href,
    "javascript:alert(1)",
  ])
    await dispatch("notificationclick", {
      notification: { close() {}, data: { url } },
    });
  assert.deepEqual(opened, [base, base, base]);
  windows = [
    {
      url: base,
      navigate: async (url) => {
        navigated.push(url);
      },
      focus: async () => {
        focused = true;
      },
    },
  ];
  await dispatch("notificationclick", {
    notification: { close() {}, data: { url: target } },
  });
  assert.deepEqual(navigated, [target]);
  assert.equal(focused, true);
  assert.equal(opened.length, 3, "existing Ivy window is reused");
}

async function openSettings(page, mobile = false) {
  if (mobile)
    await page
      .getByRole("button", { name: "Toggle navigation", exact: true })
      .click();
  await page.getByRole("button", { name: "Account", exact: true }).click();
  await page
    .getByRole("menuitem", { name: "App & notifications", exact: true })
    .click();
  const dialog = page.getByRole("dialog", {
    name: "App & notifications",
    exact: true,
  });
  await expect(dialog).toBeVisible();
  return dialog;
}

test(
  "one installable Ivy app and notification settings work across Console and published UIs on desktop and mobile",
  { timeout: 600000 },
  async (t) => {
    const f = await fixture(t, false, undefined, {}, startBrowserPushHive);
    const key = createECDH("prime256v1");
    key.generateKeys();
    const subscription = {
      endpoint: "https://fcm.googleapis.com/fcm/send/browser-fixture",
      keys: {
        auth: randomBytes(16).toString("base64url"),
        p256dh: key.getPublicKey().toString("base64url"),
      },
    };
    // Exercise real service-worker registration and Hive HTTP/storage. Only the
    // browser vendor's subscription service is replaced; this test sends no push.
    const betaSubscription = {
      ...subscription,
      endpoint: "https://jmt17.google.com/fcm/send/browser-fixture",
    };
    await f.context.addInitScript(
      ({ stable, beta }) => {
        let permission = "default";
        window.permissionRequests = 0;
        Object.defineProperty(Notification, "permission", {
          get: () => permission,
        });
        Notification.requestPermission = async () => {
          window.permissionRequests++;
          permission = "granted";
          return permission;
        };
        let current = null;
        PushManager.prototype.getSubscription = async () => current;
        PushManager.prototype.subscribe = async (options) => {
          const sub = innerWidth < 600 ? beta : stable;
          if (
            !(options.applicationServerKey instanceof Uint8Array) ||
            options.applicationServerKey.length !== 65
          )
            throw Error("Missing VAPID public key");
          current = {
            ...sub,
            options,
            toJSON: () => sub,
            unsubscribe: async () => {
              current = null;
              return true;
            },
          };
          return current;
        };
      },
      { stable: subscription, beta: betaSubscription },
    );
    const directory = resolve("dist/apps/wiki-ui"),
      definition = JSON.parse(
        await readFile(resolve(directory, "ivy-ui.json"), "utf8"),
      );
    await publishUi(f.client, {
      directory,
      definition,
      expectedReleaseId: null,
      mutationId: await newOperationId(f.client),
    });
    await f.login();
    await expect(installBanner(f.page)).toBeVisible();
    await f.page.reload();
    await expect(installBanner(f.page)).toBeVisible();
    await expect
      .poll(() =>
        f.page.evaluate(() =>
          navigator.serviceWorker
            .getRegistration()
            .then((registration) => registration?.active?.state),
        ),
      )
      .toBe("activated");
    assert.equal(await f.page.evaluate(() => window.permissionRequests), 0);
    const manifestUrl = await f.page
      .locator("link[rel=manifest]")
      .getAttribute("href");
    assert.equal(manifestUrl, f.base + "/app/manifest.webmanifest");
    const manifest = await (await f.context.request.get(manifestUrl)).json();
    assert.equal(manifest.id, "/ivy/");
    assert.equal(manifest.scope, "/ivy/");
    assert.equal(manifest.start_url, "/ivy/");
    assert.equal(manifest.display, "standalone");
    const worker = await f.context.request.get(f.base + "/app-worker.js");
    assert.equal(worker.status(), 200);
    assert.equal(worker.headers()["service-worker-allowed"], "/ivy/");
    assert.equal(worker.headers()["cache-control"], "no-store");
    await workerScope(await worker.text(), f.base + "/");
    for (const icon of manifest.icons) {
      const response = await f.context.request.get(
        new URL(icon.src, f.origin).href,
      );
      assert.equal(response.headers()["content-type"], "image/png");
      const bytes = await response.body();
      assert.equal(bytes.readUInt32BE(16), Number(icon.sizes.split("x")[0]));
    }
    let dialog = await openSettings(f.page);
    await expect(
      dialog.getByRole("button", { name: "Enable notifications" }),
    ).toBeEnabled();
    await dialog.getByRole("button", { name: "Enable notifications" }).click();
    await expect(
      dialog.getByRole("button", { name: "Send test notification" }),
    ).toBeEnabled();
    assert.equal(await f.page.evaluate(() => window.permissionRequests), 1);
    await dialog
      .getByRole("button", { name: "Send test notification" })
      .click();
    await expect
      .poll(async () => (await f.server.pushDeliveries()).length)
      .toBe(1);
    await f.server.pushStatus(503);
    await dialog
      .getByRole("button", { name: "Send test notification" })
      .click();
    await expect
      .poll(async () => {
        return f.page.evaluate(
          async ({ url, endpoint }) => {
            const response = await fetch(url, {
              method: "POST",
              credentials: "same-origin",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({ action: "status", endpoint }),
            });
            if (!response.ok)
              throw new Error("Unable to read push delivery status");
            return (await response.json()).lastError;
          },
          {
            url: f.base + "/api/browser-push",
            endpoint: subscription.endpoint,
          },
        );
      })
      .toMatch(/Push delivery failed/);
    await f.page.keyboard.press("Escape");
    dialog = await openSettings(f.page);
    await expect(dialog.getByText(/Push delivery failed/)).toBeVisible();
    await f.server.pushStatus(201);
    await dialog.getByRole("button", { name: "Disable notifications" }).click();
    await expect(
      dialog.getByRole("button", { name: "Enable notifications" }),
    ).toBeEnabled();
    await f.page.keyboard.press("Escape");
    await f.page.evaluate(() => {
      const event = new Event("beforeinstallprompt", { cancelable: true });
      event.prompt = async () => {
        window.installRequested = true;
        window.dispatchEvent(new Event("appinstalled"));
      };
      event.userChoice = Promise.resolve({ outcome: "accepted" });
      window.dispatchEvent(event);
    });
    await installBanner(f.page)
      .getByRole("button", { name: "Install app", exact: true })
      .click();
    assert.equal(await f.page.evaluate(() => window.installRequested), true);
    await expect(installBanner(f.page)).toHaveCount(0);
    await expect(f.page.getByRole("dialog")).toHaveCount(0);
    await f.page.goto(f.base + "/wiki/");
    await expect(
      f.page.getByRole("heading", { name: "Wiki", exact: true }),
    ).toBeVisible();
    await expect(installBanner(f.page)).toHaveCount(0);
    assert.equal(
      await f.page.locator("link[rel=manifest]").getAttribute("href"),
      manifestUrl,
    );
    assert.equal(
      await f.page.evaluate(() =>
        navigator.serviceWorker.getRegistrations().then((list) => list.length),
      ),
      1,
    );
    await f.page.setViewportSize({ width: 390, height: 844 });
    dialog = await openSettings(f.page, true);
    await expect(dialog.getByText(/Add to Home Screen/)).toBeVisible();
    await expect(
      dialog.getByRole("button", { name: "Enable notifications" }),
    ).toBeEnabled();
    await dialog.getByRole("button", { name: "Enable notifications" }).click();
    await expect(
      dialog.getByRole("button", { name: "Disable notifications" }),
    ).toBeEnabled();
    await dialog
      .getByRole("button", { name: "Send test notification" })
      .click();
    await expect
      .poll(async () => (await f.server.pushDeliveries()).length)
      .toBe(3);
    assert.deepEqual(
      (await f.server.pushDeliveries()).map((delivery) => delivery.endpoint),
      [subscription.endpoint, subscription.endpoint, betaSubscription.endpoint],
    );
    const endpoint = betaSubscription.endpoint;
    const cookie = async (context) =>
      (await context.cookies())
        .map(({ name, value }) => `${name}=${value}`)
        .join("; ");
    const call = async (context, data, origin = f.origin, identity) =>
      context.request.post(f.base + "/api/browser-push", {
        headers: {
          Origin: origin,
          Cookie: identity ?? (await cookie(context)),
        },
        data,
      });
    const other = await f.browser.newContext();
    t.after(() => other.close());
    assert.equal(
      (await call(other, { action: "test", endpoint })).status(),
      401,
    );
    assert.equal(
      (
        await call(
          f.context,
          { action: "test", endpoint },
          "https://elsewhere.test",
        )
      ).status(),
      401,
    );
    const login = await other.request.post(f.base + "/login", {
      headers: { Origin: f.origin },
      form: { token: fixtureToken },
      maxRedirects: 0,
    });
    assert.equal(login.status(), 303);
    const otherCookie = login.headers()["set-cookie"].split(";")[0];
    assert.equal(
      (
        await call(
          other,
          { action: "unsubscribe", endpoint },
          f.origin,
          otherCookie,
        )
      ).status(),
      200,
    );
    const status = await (
      await call(f.context, { action: "status", endpoint })
    ).json();
    assert.equal(status.subscribed, true);
    assert.ok(status.publicKey);
    assert.equal(status.privateKey, undefined);
    assert.equal(
      (
        await call(other, { action: "test", endpoint }, f.origin, otherCookie)
      ).status(),
      404,
    );
    assert.ok(
      await f.page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth + 1,
      ),
    );
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
    let ready = false;
    const service = new ServiceClient({
      publicBaseUrl: f.base,
      credential: () => fixtureToken,
      identity: {
        serviceNodeId: "push-fixture",
        hostId: "fixture",
        serviceName: "push-fixture",
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
        if (value.status === "ready") ready = true;
      },
    });
    t.after(() => service.stop());
    service.start();
    await expect.poll(() => ready, { timeout: 15000 }).toBe(true);
    await Promise.all(f.context.pages().map((page) => page.close()));
    await publishBrowserNotice(service.connection, "fixture", {
      title: "Result",
      body: "Finished",
      tag: "without-a-page",
      target: { uiId: "agent-ui", fragment: "#/thread?id=thread+%26+one" },
    });
    await expect
      .poll(async () => (await f.server.pushDeliveries()).length)
      .toBe(4);
    assert.match(
      (await f.server.pushDeliveries())[3].payload.url,
      /ui\/agent-ui/,
    );
    const logout = await f.context.request.post(f.base + "/logout", {
      headers: { Origin: f.origin, Cookie: await cookie(f.context) },
      maxRedirects: 0,
    });
    assert.equal(logout.status(), 303);
    assert.equal(
      (await call(f.context, { action: "test", endpoint })).status(),
      401,
    );
    await service.stop();
    await other.close();
    assert.deepEqual(await f.server.close(), { code: 0, signal: null });
    assert.equal(
      JSON.parse(
        await readFile(
          resolve(f.server.config.dataRoot, "health.json"),
          "utf8",
        ),
      ).ready,
      false,
    );
  },
);

test(
  "mobile install help stays dismissible, remembers the choice across tabs and keeps other browser profiles independent",
  { timeout: 90000 },
  async (t) => {
    const f = await fixture(t, false, { width: 390, height: 844 });
    await f.login();
    await expect(installBanner(f.page)).toBeVisible();
    assert.ok(
      await f.page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth + 1,
      ),
    );
    await installBanner(f.page)
      .getByRole("button", { name: "How to install", exact: true })
      .click();
    const dialog = f.page.getByRole("dialog", {
      name: "App & notifications",
      exact: true,
    });
    await expect(dialog.getByText(/Add to Home Screen/)).toBeVisible();
    await f.page.keyboard.press("Escape");
    await expect(installBanner(f.page)).toBeVisible();
    const sibling = await f.context.newPage();
    await sibling.goto(f.base + "/");
    await expect(installBanner(sibling)).toBeVisible();
    await installBanner(f.page)
      .getByRole("button", { name: "Dismiss installation banner" })
      .click();
    await expect(installBanner(f.page)).toHaveCount(0);
    await expect(installBanner(sibling)).toHaveCount(0);
    await f.page.reload();
    await expect(
      f.page.getByRole("heading", { name: "UIs", exact: true }),
    ).toBeVisible();
    await expect(installBanner(f.page)).toHaveCount(0);

    const other = await f.browser.newContext({
      viewport: { width: 1440, height: 1000 },
    });
    t.after(() => other.close());
    await other.addCookies(await f.context.cookies());
    const page = await other.newPage();
    await page.goto(f.base + "/");
    await expect(installBanner(page)).toBeVisible();
    await page.evaluate(() => {
      const event = new Event("beforeinstallprompt", { cancelable: true });
      event.prompt = async () => {};
      event.userChoice = Promise.resolve({ outcome: "dismissed" });
      window.dispatchEvent(event);
    });
    await installBanner(page)
      .getByRole("button", { name: "Install app", exact: true })
      .click();
    await expect(installBanner(page)).toHaveCount(0);
    await page.reload();
    await expect(
      page.getByRole("heading", { name: "UIs", exact: true }),
    ).toBeVisible();
    await expect(installBanner(page)).toHaveCount(0);
    assert.deepEqual(f.pageErrors, []);
  },
);

test(
  "installed apps suppress the banner on later browser visits",
  { timeout: 90000 },
  async (t) => {
    for (const mode of ["standalone", "appinstalled"])
      await t.test(mode, async (t) => {
        const f = await fixture(t);
        if (mode === "standalone")
          await f.context.addInitScript(() => {
            Object.defineProperty(navigator, "standalone", {
              get: () =>
                sessionStorage.getItem("fixture:display-mode") !== "browser",
            });
          });
        await f.login();
        if (mode === "appinstalled") {
          await expect(installBanner(f.page)).toBeVisible();
          await f.page.evaluate(() =>
            window.dispatchEvent(new Event("appinstalled")),
          );
        }
        await expect(installBanner(f.page)).toHaveCount(0);
        await f.page.evaluate(() =>
          sessionStorage.setItem("fixture:display-mode", "browser"),
        );
        await f.page.reload();
        await expect(
          f.page.getByRole("heading", { name: "UIs", exact: true }),
        ).toBeVisible();
        await expect(installBanner(f.page)).toHaveCount(0);
        assert.deepEqual(f.pageErrors, []);
      });
  },
);

test(
  "blocked notifications explain recovery without prompting automatically",
  { timeout: 60000 },
  async (t) => {
    const f = await fixture(t);
    await f.context.addInitScript(() =>
      Object.defineProperty(Notification, "permission", {
        get: () => "denied",
      }),
    );
    await f.login();
    const dialog = await openSettings(f.page);
    await expect(dialog.getByText(/Notifications are blocked/)).toBeVisible();
    await expect(
      dialog.getByRole("button", { name: "Enable notifications" }),
    ).toHaveCount(0);
    assert.deepEqual(f.pageErrors, []);
  },
);
