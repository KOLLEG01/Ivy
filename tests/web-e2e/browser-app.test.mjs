import test from "node:test";
import assert from "node:assert/strict";
import { createECDH, randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { expect } from "@playwright/test";
import { fixture } from "./fixtures/fixture.mjs";
import { publishUi } from "../../dist/packages/cli/src/publish-ui.js";
import { newOperationId } from "../../dist/packages/sdk/src/client.js";

const installBanner = (page) =>
  page.getByRole("region", { name: "Install Ivy", exact: true });

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
  { timeout: 90000 },
  async (t) => {
    const sent = [];
    const deliveryEndpoints = [];
    let deliveryFails = false;
    const f = await fixture(t, false, undefined, {
      sendBrowserPush: async (subscription, payload) => {
        deliveryEndpoints.push(subscription.endpoint);
        sent.push(JSON.parse(payload));
        return { statusCode: deliveryFails ? 503 : 201, headers: {}, body: "" };
      },
    });
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
    assert.equal(manifest.scope, "/ivy/");
    assert.equal(manifest.display, "standalone");
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
    await expect.poll(() => sent.length).toBe(1);
    deliveryFails = true;
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
    deliveryFails = false;
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
    await expect.poll(() => sent.length).toBe(3);
    assert.deepEqual(deliveryEndpoints, [
      subscription.endpoint,
      subscription.endpoint,
      betaSubscription.endpoint,
    ]);
    assert.ok(
      await f.page.evaluate(
        () => document.documentElement.scrollWidth <= innerWidth + 1,
      ),
    );
    assert.deepEqual(f.pageErrors, []);
    assert.deepEqual(f.externalRequests, []);
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
