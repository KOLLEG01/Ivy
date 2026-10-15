import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import { createServer } from "node:http";
import test from "node:test";
import capture, { maximumWebhookBytes, verifyWebhookSignature, verifyWebhookChallenge } from "../docs/examples/data-collector/social/instagram-webhook.mjs";
import { instagramWebhookEndpoint } from "../docs/examples/data-collector/social/instagram-webhook-endpoint.mjs";

const now = Date.UTC(2026, 9, 14, 10);
const appSecret = "fixture-signing-secret";
const verifyToken = "fixture-verification-token";
const config = { appId: "456", accountId: "123", accountScopedId: "321", appSecretSecret: "signing" };
const secrets = { signing: appSecret };
const sign = (body) => `sha256=${createHmac("sha256", appSecret).update(body).digest("hex")}`;
const comment = (id, text = "A comment") => ({ id, text, from: { id: "987", username: "reader" }, media: { id: "100" } });
const envelope = (entries) => ({ object: "instagram", entry: entries });
const input = (payload) => {
  const body = Buffer.from(JSON.stringify(payload));
  return { schemaVersion: 1, receivedAt: new Date(now).toISOString(), rawBodyBase64: body.toString("base64"), signature: sign(body) };
};

test("Instagram webhook verification uses raw bytes, independent secrets and an unambiguous challenge", () => {
  const body = Buffer.from('{ "object": "instagram", "text": "Grüße" }');
  const signature = sign(body);
  assert.equal(verifyWebhookSignature(body, signature, appSecret), true);
  assert.equal(verifyWebhookSignature(Buffer.from(JSON.stringify(JSON.parse(body))), signature, appSecret), false);
  assert.equal(verifyWebhookSignature(body, signature, verifyToken), false);
  for (const malformed of [null, "sha1=abc", "sha256=abc", [signature]])
    assert.equal(verifyWebhookSignature(body, malformed, appSecret), false);
  assert.equal(verifyWebhookSignature(Buffer.alloc(maximumWebhookBytes + 1), signature, appSecret), false);
  const parameters = new URLSearchParams({ "hub.mode": "subscribe", "hub.verify_token": verifyToken, "hub.challenge": "001234" });
  assert.equal(verifyWebhookChallenge(parameters, verifyToken), "001234");
  parameters.append("hub.verify_token", verifyToken);
  assert.equal(verifyWebhookChallenge(parameters, verifyToken), null);
});

test("Instagram webhook capture handles both official comment shapes, account separation and replay", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now });
  const original = comment("1");
  const payload = [envelope([
    { id: "999", time: now / 1000, field: "comments", value: comment("foreign") },
    { id: "123", time: now / 1000, field: "comments", value: original },
    { id: "123", time: now / 1000, changes: [{ field: "comments", value: { ...comment("2"), id: undefined, comment_id: "2", parent_id: "1" } }] },
  ])];
  const first = await capture({ config, secrets, input: input(payload) });
  assert.deepEqual(first.data.items.map((item) => item.id), ["1", "2"]);
  assert.equal(first.data.delivery.matchingEntries, 2);
  assert.equal(first.data.items[0].createdAt, null);
  assert.equal(first.data.items[1].parentId, "1");
  assert.equal(first.events[0].payload.comments, 2);
  assert.equal(first.data.unreadMessages.value, null);
  assert.equal(JSON.stringify(first).includes(appSecret), false);
  assert.equal(JSON.stringify(first.events).includes("A comment"), false);
  assert.equal(JSON.stringify(first.state).includes("A comment"), false);
  const reordered = { media: original.media, from: original.from, text: original.text, id: original.id };
  const replay = await capture({ config, secrets, state: first.state, input: input(envelope([
    { id: "123", time: now / 1000 + 1, field: "comments", value: reordered },
  ])) });
  assert.deepEqual(replay.events, []);
  const edit = await capture({ config, secrets, state: replay.state, input: input(envelope([
    { id: "123", time: now / 1000 + 2, field: "comments", value: { ...original, text: "Edited" } },
  ])) });
  assert.equal(edit.events[0].payload.comments, 1);
});

test("Instagram webhook messages suppress echoes, retain unsupported events and reject foreign routing", async () => {
  const incoming = { sender: { id: "987" }, recipient: { id: "123" }, timestamp: now,
    message: { mid: "incoming", text: "Private message" } };
  const payload = envelope([{ id: "123", time: now / 1000, messaging: [incoming,
    { sender: { id: "321" }, recipient: { id: "987" }, timestamp: now, message: { mid: "outgoing", is_echo: true, text: "Reply" } },
    { sender: { id: "987" }, recipient: { id: "123" }, timestamp: now, read: { mid: "incoming" } },
  ] }]);
  const result = await capture({ config, secrets, input: input(payload) });
  assert.equal(result.events[0].payload.directMessages, 1);
  assert.equal(result.data.items[1].direction, "outgoing");
  assert.equal(result.data.coverage.complete, false);
  assert.equal(result.data.coverage.unhandled[0].field, "read");
  assert.equal(result.data.unreadMessages.status, "unavailable");
  await assert.rejects(capture({ config, secrets, input: input(envelope([{ id: "123", messaging: [
    { ...incoming, recipient: { id: "555" } },
  ] }])) }), { code: "authentication_required" });
  const forged = input(payload);
  forged.rawBodyBase64 = Buffer.from(JSON.stringify(envelope([]))).toString("base64");
  await assert.rejects(capture({ config, secrets, input: forged }), { code: "authentication_required" });
  await assert.rejects(capture({ config, secrets, input: input(envelope([
    { id: "999", field: "comments", value: comment("foreign") },
  ])) }), { code: "authentication_required" });
});

test("Instagram webhook events fit the Hive envelope budget and report bounded deduplication", async (t) => {
  t.mock.timers.enable({ apis: ["Date"], now });
  const payload = envelope([{ id: "123", time: now / 1000, changes: Array.from({ length: 100 }, (_, index) =>
    ({ field: "comments", value: comment(String(index)) })) }]);
  const seen = Object.fromEntries(Array.from({ length: 1000 }, (_, index) => [String(index).padStart(64, "a"), now]));
  const result = await capture({ config: { ...config, maximumSeen: 1000 }, secrets,
    state: { webhook: { scope: "456:123", seen } }, input: input(payload) });
  assert.equal(result.events[0].payload.comments, 100);
  assert.equal(result.events[0].payload.changeIds.length, 32);
  assert.equal(result.events[0].payload.additionalChanges, 68);
  assert.equal(result.data.deduplication.complete, false);
  assert.equal(Object.keys(result.state.webhook.seen).length, 1000);
  assert.ok(Buffer.byteLength(JSON.stringify({ kind: "collected", taskId: "x".repeat(64), runId: "x".repeat(36),
    eventIndex: 0, ...result.events[0] })) <= 4096);
});

test("Instagram webhook endpoint acknowledges only after durable admission and preserves signed bytes", async (t) => {
  let complete;
  let admitted;
  const queueEntered = new Promise((resolve) => { admitted = resolve; });
  const durable = new Promise((resolve) => { complete = resolve; });
  let stored;
  const handler = instagramWebhookEndpoint({ verifyToken, appSecret, accountIds: ["123"], persistDelivery: async (delivery, metadata) => {
    stored = { delivery, metadata };
    admitted();
    await durable;
  } });
  const server = createServer(handler);
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });
  t.after(() => { server.closeAllConnections(); server.close(); });
  const base = `http://127.0.0.1:${server.address().port}/instagram/webhooks`;
  const challenge = await fetch(`${base}?hub.mode=subscribe&hub.verify_token=${verifyToken}&hub.challenge=00123`);
  assert.equal(challenge.status, 200);
  assert.equal(await challenge.text(), "00123");
  const body = Buffer.from(JSON.stringify(envelope([{ id: "123", field: "comments", value: comment("1") }])));
  const invalid = await fetch(base, { method: "POST", body, headers: { "content-type": "application/json" } });
  assert.equal(invalid.status, 403);
  let responded = false;
  const pending = fetch(base, { method: "POST", body, headers: { "content-type": "application/json", "x-hub-signature-256": sign(body) } })
    .then((response) => { responded = true; return response; });
  await queueEntered;
  assert.equal(responded, false);
  assert.equal(stored.delivery.rawBodyBase64, body.toString("base64"));
  assert.deepEqual(stored.metadata.accountIds, ["123"]);
  complete();
  assert.equal((await pending).status, 200);
  const captured = await capture({ config, secrets, input: stored.delivery });
  assert.equal(captured.data.items[0].id, "1");
});

test("Instagram webhook endpoint refuses acknowledgement when private storage fails", async (t) => {
  const handler = instagramWebhookEndpoint({ verifyToken, appSecret, accountIds: ["123"], persistDelivery: async () => { throw new Error("Queue unavailable"); } });
  const server = createServer(handler);
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });
  t.after(() => { server.closeAllConnections(); server.close(); });
  const body = Buffer.from(JSON.stringify(envelope([{ id: "123", field: "comments", value: comment("1") }])));
  const response = await fetch(`http://127.0.0.1:${server.address().port}/instagram/webhooks`, { method: "POST", body,
    headers: { "content-type": "application/json", "x-hub-signature-256": sign(body) } });
  assert.equal(response.status, 503);
});
