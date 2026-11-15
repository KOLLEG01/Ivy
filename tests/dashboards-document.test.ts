import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { runInNewContext } from "node:vm";
import { dashboardDocument } from "../packages/ui-client/src/dashboard/document.js";

test("browser and native dashboard documents share exact bytes and UTF-16 channels", () => {
  const fixtures = JSON.parse(
    readFileSync(new URL("../../tests/fixtures/dashboards/document.json", import.meta.url), "utf8"),
  ) as { html: string; channel: string; document: string }[];
  for (const fixture of fixtures) {
    assert.equal(dashboardDocument(fixture.html, fixture.channel), fixture.document);
  }
});

test("a resize preserves the newest data while a slow drawing callback is running", async () => {
  type Update = { data?: unknown; output?: unknown; sequence: number };
  type Message = { source: unknown; data: Update & { channel: string; type: string } };
  type Drawing = { data: unknown; output: unknown };
  const messages: { type: string; sequence?: number }[] = [];
  const parent = { postMessage: (message: (typeof messages)[number]) => messages.push(message) };
  const listeners = new Map<string, (event: Message) => Promise<void>>();
  const context = {
    window: null as unknown,
    parent,
    document: { fonts: { ready: Promise.resolve() }, images: [] },
    requestAnimationFrame: (callback: () => void) => queueMicrotask(callback),
    addEventListener: (name: string, listener: (event: Message) => Promise<void>) => listeners.set(name, listener),
    dashboard: undefined as unknown as Drawing & { onUpdate: (handler: (value: Drawing) => Promise<void>) => void },
  };
  context.window = context;
  const script = dashboardDocument("", "test-channel").match(/<script>([\s\S]*?)<\/script>/)?.[1];
  assert.ok(script);
  runInNewContext(script, context);
  const message = listeners.get("message");
  assert.ok(message);
  const send = (update: Update, source: unknown = parent, channel = "test-channel") =>
    message({ source, data: { ...update, channel, type: "update" } });
  const firstData = { value: "first" }, latestData = { value: "latest" };
  const firstOutput = { width: 100 }, latestOutput = { width: 300 };
  const calls: Drawing[] = [];
  let active = 0, maximumActive = 0, release!: () => void;
  const blocked = new Promise<void>((resolve) => { release = resolve; });
  context.dashboard.onUpdate(async (value) => {
    maximumActive = Math.max(maximumActive, ++active);
    calls.push(value);
    if (calls.length === 1) await blocked;
    active--;
  });

  const first = send({ data: firstData, output: firstOutput, sequence: 1 });
  await send({ data: latestData, output: { width: 200 }, sequence: 2 });
  await send({ output: latestOutput, sequence: 3 });
  await send({ data: { value: "foreign" }, sequence: 4 }, {});
  await send({ data: { value: "wrong-channel" }, sequence: 5 }, parent, "other-channel");
  release();
  await first;

  assert.equal(maximumActive, 1);
  assert.equal(calls.length, 2);
  assert.equal(calls[0]?.data, firstData);
  assert.equal(calls[0]?.output, firstOutput);
  assert.equal(calls[1]?.data, latestData);
  assert.equal(calls[1]?.output, latestOutput);
  assert.deepEqual(messages.map(({ type, sequence }) => ({ type, sequence })), [
    { type: "rendered", sequence: 1 }, { type: "rendered", sequence: 3 },
  ]);

  const resized = { width: 400 };
  await send({ output: resized, sequence: 6 });
  assert.equal(calls[2]?.data, latestData);
  assert.equal(context.dashboard.data, latestData);
  assert.equal(context.dashboard.output, resized);
});
