import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { unzipSync, strFromU8 } from "fflate";
import { expect } from "@playwright/test";
import { fixture } from "./fixtures/fixture.mjs";

test(
  "Settings exports the entered OpenAI app with complete plugin assets on desktop and mobile",
  { timeout: 60000 },
  async (t) => {
    const f = await fixture(t);
    await f.login();
    const appId = "asdk_app_0123456789abcdef0123456789abcdef";
    const name = "dev-0123456789abcdef0123456789abcdef";
    const cases = [
      {
        locale: "en-US",
        width: 1440,
        title: "Connect OpenAI",
        download: "Download plugin package",
        guide: "Setup guide",
        copy: "Copy URL",
        copied: "Copied",
      },
      {
        locale: "de-DE",
        width: 390,
        title: "OpenAI anbinden",
        download: "Plugin-Paket herunterladen",
        guide: "Kurzanleitung",
        copy: "URL kopieren",
        copied: "Kopiert",
      },
    ];
    for (const scenario of cases) {
      const context = await f.browser.newContext({
        locale: scenario.locale,
        viewport: { width: scenario.width, height: 1000 },
        storageState: await f.context.storageState(),
        acceptDownloads: true,
      });
      try {
        const page = await context.newPage(),
          errors = [];
        page.on("pageerror", (error) => errors.push(error.message));
        await page.goto(f.base + "/#/system/settings");
        const section = page.getByRole("region", {
          name: scenario.title,
          exact: true,
        });
        await expect(section).toBeVisible();
        await expect(
          section.getByLabel("MCP-URL", { exact: true }),
        ).toHaveValue(f.base + "/mcp");
        const input = section.getByLabel("OpenAI App-ID", { exact: true });
        const button = section.getByRole("button", {
          name: scenario.download,
          exact: true,
        });
        await expect(button).toBeDisabled();
        for (const invalid of [
          "not-an-app",
          "asdk_app_123",
          "https://example.invalid/plugins/plugin_" + appId,
        ]) {
          await input.fill(invalid);
          await expect(input).toHaveAttribute("aria-invalid", "true");
          await expect(button).toBeDisabled();
        }
        for (const valid of [
          appId,
          "plugin_" + appId,
          "https://chatgpt.com/plugins#settings/Plugins/plugin_" + appId,
        ]) {
          await input.fill(valid);
          await expect(button).toBeEnabled();
        }
        await expect(
          section.getByRole("button", { name: scenario.guide, exact: true }),
        ).toHaveAttribute("aria-expanded", "true");
        await expect(section.locator("ol li")).toHaveCount(5);
        assert.ok(
          await page.evaluate(
            () => document.documentElement.scrollWidth <= innerWidth + 1,
          ),
        );
        await context.grantPermissions(["clipboard-read", "clipboard-write"], {
          origin: f.origin,
        });
        await section
          .getByRole("button", { name: scenario.copy, exact: true })
          .click();
        await expect(
          section.getByRole("button", { name: scenario.copied, exact: true }),
        ).toBeVisible();
        assert.equal(
          await page.evaluate(() => navigator.clipboard.readText()),
          f.base + "/mcp",
        );

        const downloading = page.waitForEvent("download");
        await button.click();
        const download = await downloading;
        assert.match(
          download.suggestedFilename(),
          /^ivy-openai-plugin-\d+\.\d+\.\d+\.zip$/,
        );
        const destination = resolve(f.root, scenario.locale + ".zip");
        await download.saveAs(destination);
        const files = unzipSync(await readFile(destination));
        assert.deepEqual(
          Object.keys(files).sort(),
          [
            ".app.json",
            ".codex-plugin/plugin.json",
            "assets/icon.png",
            "assets/logo.png",
          ].sort(),
        );
        const manifest = JSON.parse(
          strFromU8(files[".codex-plugin/plugin.json"]),
        );
        assert.equal(manifest.name, name);
        assert.equal(manifest.interface.displayName, "Ivy");
        assert.equal(manifest.apps, "./.app.json");
        assert.deepEqual(JSON.parse(strFromU8(files[".app.json"])), {
          apps: { [name]: { id: appId } },
        });
        for (const asset of ["icon", "logo"]) {
          assert.deepEqual(
            Buffer.from(files[`assets/${asset}.png`]),
            await readFile(`services/hive/chatgpt-plugin/assets/${asset}.png`),
          );
        }
        await expect(section.getByRole("alert")).toHaveCount(0);
        assert.deepEqual(errors, []);
      } finally {
        await context.close();
      }
    }
  },
);
