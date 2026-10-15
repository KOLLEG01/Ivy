import { createHash } from "node:crypto";
import { maximumWebhookBytes, parseWebhookEnvelope, verifyWebhookChallenge, verifyWebhookSignature } from "./instagram-webhook.mjs";

/** HTTP adapter factory. The caller supplies a durable, private delivery queue. */
export function instagramWebhookEndpoint({ path = "/instagram/webhooks", verifyToken, appSecret, accountIds, persistDelivery }) {
  if (typeof path !== "string" || !/^\/[A-Za-z0-9/_-]+$/.test(path) ||
      typeof verifyToken !== "string" || !verifyToken || typeof appSecret !== "string" || !appSecret ||
      !Array.isArray(accountIds) || accountIds.length < 1 || accountIds.some((id) => typeof id !== "string" || !/^[0-9]{1,30}$/.test(id)) ||
      typeof persistDelivery !== "function") throw new Error("Configure the callback, secrets, accounts and durable delivery queue.");
  const accounts = new Set(accountIds);
  return async (request, response) => {
    response.setHeader("Cache-Control", "no-store");
    response.setHeader("X-Content-Type-Options", "nosniff");
    let url;
    try { url = new URL(request.url, "http://localhost"); } catch { response.writeHead(400).end(); return; }
    if (url.pathname !== path) { response.writeHead(404).end(); request.resume(); return; }
    if (request.method === "GET") {
      const challenge = verifyWebhookChallenge(url.searchParams, verifyToken);
      response.writeHead(challenge === null ? 403 : 200, { "Content-Type": "text/plain; charset=utf-8" }).end(challenge ?? "");
      request.resume();
      return;
    }
    if (request.method !== "POST") { response.writeHead(405).end(); request.resume(); return; }
    if (!/^application\/json(?:\s*;|$)/i.test(request.headers["content-type"] ?? "") ||
        (request.headers["content-encoding"] && request.headers["content-encoding"] !== "identity")) {
      response.writeHead(415).end(); request.resume(); return;
    }
    const chunks = [];
    let bytes = 0;
    try {
      for await (const chunk of request) {
        bytes += chunk.length;
        if (bytes > maximumWebhookBytes) { response.writeHead(413).end(); request.resume(); return; }
        chunks.push(chunk);
      }
    } catch { response.writeHead(400).end(); return; }
    const rawBody = Buffer.concat(chunks);
    const signature = request.headers["x-hub-signature-256"];
    if (!verifyWebhookSignature(rawBody, signature, appSecret)) { response.writeHead(403).end(); return; }
    let entries;
    try { entries = parseWebhookEnvelope(rawBody); } catch { response.writeHead(400).end(); return; }
    const matched = [...new Set(entries.filter((entry) => accounts.has(entry.id)).map((entry) => entry.id))];
    if (matched.length) {
      try {
        // Resolve only after durable storage. A successful collector admission is
        // not confirmation that its result has committed.
        await persistDelivery({ schemaVersion: 1, receivedAt: new Date().toISOString(),
          rawBodyBase64: rawBody.toString("base64"), signature }, {
          deliveryId: createHash("sha256").update(rawBody).digest("hex"), accountIds: matched,
        });
      } catch { response.writeHead(503).end(); return; }
    }
    response.writeHead(200, { "Content-Type": "text/plain; charset=utf-8" }).end("EVENT_RECEIVED");
  };
}
