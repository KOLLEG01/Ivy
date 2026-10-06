/// <reference path="./pngjs.d.ts" />
import { chromium } from "playwright-core";
import type { Browser } from "playwright-core";
import { PNG } from "pngjs";
import { randomUUID } from "node:crypto";
import { requireThat } from "../../../packages/sdk/src/node.js";
import { dashboardDocument } from "./document.js";
import type { OutputOptions } from "./schema.js";

export function convertPng(
  bytes: Buffer,
  mode: OutputOptions["colorMode"],
): Buffer {
  if (mode === "color") return bytes;
  const png = PNG.sync.read(bytes);
  for (let i = 0; i < png.data.length; i += 4) {
    const alpha = png.data[i + 3]! / 255;
    const luminance = Math.round(
      (0.2126 * png.data[i]! +
        0.7152 * png.data[i + 1]! +
        0.0722 * png.data[i + 2]!) *
        alpha +
        255 * (1 - alpha),
    );
    const value =
      mode === "monochrome" ? (luminance >= 128 ? 255 : 0) : luminance;
    png.data[i] = png.data[i + 1] = png.data[i + 2] = value;
    png.data[i + 3] = 255;
  }
  return PNG.sync.write(png, { bitDepth: 8, colorType: 0, inputColorType: 6 });
}

export class DashboardRenderer {
  private browser: Browser | null = null;
  private busy = false;
  constructor(readonly executablePath?: string) {}
  async start() {
    if (!this.browser) {
      const browser = await chromium.launch({
        headless: true,
        chromiumSandbox: true,
        ...(this.executablePath ? { executablePath: this.executablePath } : {}),
      });
      this.browser = browser;
      browser.on("disconnected", () => {
        if (this.browser === browser) this.browser = null;
      });
    }
  }
  async close() {
    await this.browser?.close();
    this.browser = null;
  }
  async render(html: string, data: unknown, options: OutputOptions) {
    requireThat(
      !this.busy,
      "resource_busy",
      "An image is already rendering; retry shortly.",
    );
    this.busy = true;
    let context;
    try {
      await this.start();
      context = await this.browser!.newContext({
        viewport: { width: options.width, height: options.height },
        deviceScaleFactor: 1,
        serviceWorkers: "block",
      });
      await context.route("**/*", (route) => route.abort());
      const page = await context.newPage();
      page.setDefaultTimeout(15000);
      const channel = randomUUID();
      await page.setContent(
        '<html><body style="margin:0"><iframe sandbox="allow-scripts" style="border:0;width:100vw;height:100vh;display:block"></iframe></body></html>',
      );
      await page.evaluate(
        ({ document, channel, data, options }) => {
          const frame = window.document.querySelector("iframe")!;
          (window as any).renderState = "loading";
          window.addEventListener("message", (event) => {
            if (
              event.source !== frame.contentWindow ||
              event.data?.channel !== channel
            )
              return;
            if (event.data.type === "ready")
              frame.contentWindow!.postMessage(
                {
                  channel,
                  type: "update",
                  sequence: 1,
                  data,
                  output: { ...options, mode: "image" },
                },
                "*",
              );
            if (event.data.type === "rendered")
              (window as any).renderState = "done";
            if (event.data.type === "error") {
              (window as any).renderState = "error";
              (window as any).renderError = event.data.message;
            }
          });
          frame.srcdoc = document;
        },
        { document: dashboardDocument(html, channel), channel, data, options },
      );
      await page.waitForFunction(
        () => (window as any).renderState !== "loading",
      );
      const { state, error } = await page.evaluate(() => ({
        state: (window as any).renderState as string,
        error: (window as any).renderError as string | undefined,
      }));
      requireThat(
        state === "done",
        "dashboard_render_failed",
        error || "Dashboard rendering failed.",
      );
      const png = convertPng(
        await page.screenshot({
          type: "png",
          animations: "disabled",
          timeout: 15000,
        }),
        options.colorMode,
      );
      requireThat(
        png.length <= 8 * 1024 * 1024,
        "resource_exhausted",
        "PNG exceeds 8 MiB; reduce dimensions.",
      );
      return {
        mimeType: "image/png",
        encoding: "base64",
        data: png.toString("base64"),
        ...options,
      };
    } finally {
      try {
        await context?.close();
      } finally {
        this.busy = false;
      }
    }
  }
}
