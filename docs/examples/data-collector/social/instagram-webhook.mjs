import { createHash, createHmac, timingSafeEqual } from "node:crypto";

export const maximumWebhookBytes = 131072;
const record = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
const failure = (code, message) => Object.assign(new Error(message), { code });
const digest = (value) => createHash("sha256").update(value).digest("hex");
const canonical = (value) => Array.isArray(value) ? `[${value.map(canonical).join(",")}]` :
  record(value) ? `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}` :
    JSON.stringify(value);
const identifier = (value) => typeof value === "string" && value.length > 0 && value.length <= 1024;
const timestamp = (value, multiplier) => {
  const milliseconds = value * multiplier;
  return Number.isSafeInteger(milliseconds) && milliseconds >= 0 && milliseconds <= 8640000000000000
    ? new Date(milliseconds).toISOString()
    : null;
};

/** Verify the exact bytes received from Meta, before parsing or reserialization. */
export function verifyWebhookSignature(rawBody, signature, appSecret) {
  if (!Buffer.isBuffer(rawBody) || rawBody.length === 0 || rawBody.length > maximumWebhookBytes ||
      typeof signature !== "string" || !/^sha256=[a-fA-F0-9]{64}$/.test(signature) ||
      typeof appSecret !== "string" || !appSecret) return false;
  const supplied = Buffer.from(signature.slice(7), "hex");
  const expected = createHmac("sha256", appSecret).update(rawBody).digest();
  return timingSafeEqual(supplied, expected);
}

/** Return the literal challenge only for a single, matching verification request. */
export function verifyWebhookChallenge(parameters, verifyToken) {
  if (!(parameters instanceof URLSearchParams) || typeof verifyToken !== "string" || !verifyToken ||
      ["hub.mode", "hub.verify_token", "hub.challenge"].some((key) => parameters.getAll(key).length !== 1) ||
      parameters.get("hub.mode") !== "subscribe") return null;
  const supplied = Buffer.from(parameters.get("hub.verify_token"));
  const expected = Buffer.from(verifyToken);
  const challenge = parameters.get("hub.challenge");
  return supplied.length === expected.length && timingSafeEqual(supplied, expected) &&
    /^[0-9]{1,64}$/.test(challenge) ? challenge : null;
}

export function parseWebhookEnvelope(rawBody) {
  if (!Buffer.isBuffer(rawBody) || rawBody.length === 0 || rawBody.length > maximumWebhookBytes)
    throw failure("configuration_invalid", "The webhook payload is missing or exceeds the input limit.");
  let payload;
  try { payload = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(rawBody)); }
  catch { throw failure("configuration_invalid", "The webhook payload is not valid UTF-8 JSON."); }
  // Meta's examples show both a single notification and an array of notifications.
  const notifications = Array.isArray(payload) ? payload : [payload];
  if (notifications.length < 1 || notifications.length > 1000 || notifications.some((item) =>
    !record(item) || item.object !== "instagram" || !Array.isArray(item.entry) ||
    item.entry.length > 1000 || item.entry.some((entry) => !record(entry) || !identifier(entry.id))))
    throw failure("configuration_invalid", "The payload is not an Instagram webhook envelope.");
  const entries = notifications.flatMap((item) => item.entry);
  if (entries.length > 1000)
    throw failure("configuration_invalid", "The webhook contains too many entries.");
  return entries;
}

/** A separate input-only task captures one configured professional account. */
export default async function captureInstagramWebhook({ config, secrets, state, input, signal }) {
  signal?.throwIfAborted();
  if (!record(config) || typeof config.accountId !== "string" || !/^[0-9]{1,30}$/.test(config.accountId) ||
      typeof config.appId !== "string" || !/^[0-9]{1,30}$/.test(config.appId) ||
      typeof config.appSecretSecret !== "string" || !/^[A-Za-z][A-Za-z0-9_]{0,127}$/.test(config.appSecretSecret) ||
      (config.accountScopedId !== undefined && (typeof config.accountScopedId !== "string" || !/^[0-9]{1,30}$/.test(config.accountScopedId))))
    throw failure("configuration_invalid", "Configure the app, professional account and webhook signing-secret reference.");
  const secret = secrets?.[config.appSecretSecret];
  if (typeof secret !== "string" || !secret)
    throw failure("authentication_required", "The configured webhook signing secret is missing.");
  if (!record(input) || input.schemaVersion !== 1 || typeof input.rawBodyBase64 !== "string" ||
      input.rawBodyBase64.length > Math.ceil(maximumWebhookBytes / 3) * 4 ||
      typeof input.receivedAt !== "string" || !Number.isFinite(Date.parse(input.receivedAt)))
    throw failure("configuration_invalid", "The webhook input envelope is invalid.");
  const rawBody = Buffer.from(input.rawBodyBase64, "base64");
  if (rawBody.toString("base64") !== input.rawBodyBase64 ||
      !verifyWebhookSignature(rawBody, input.signature, secret))
    throw failure("authentication_required", "The webhook signature is invalid.");
  const entries = parseWebhookEnvelope(rawBody).filter((entry) => entry.id === config.accountId);
  if (!entries.length)
    throw failure("authentication_required", "The webhook does not include the configured account.");
  const scope = `${config.appId}:${config.accountId}`;
  const collectedAt = new Date().toISOString();
  const retentionDays = config.deduplicationDays ?? 7;
  const maximumSeen = config.maximumSeen ?? 2000;
  if (!Number.isInteger(retentionDays) || retentionDays < 2 || retentionDays > 30 ||
      !Number.isInteger(maximumSeen) || maximumSeen < 1000 || maximumSeen > 5000)
    throw failure("configuration_invalid", "Webhook deduplication limits are invalid.");
  const cutoff = Date.now() - retentionDays * 86400000;
  const previous = state?.webhook?.scope === scope ? state.webhook : null;
  const seen = Object.fromEntries(Object.entries(previous?.seen ?? {}).filter(([key, at]) =>
    /^[a-f0-9]{64}$/.test(key) && Number.isSafeInteger(at) && at >= cutoff));
  const ownerIds = new Set([config.accountId, config.accountScopedId].filter(Boolean));
  const items = [], changes = [], unhandled = [];
  const uniqueItems = new Set();
  const capture = (item, semantic) => {
    const changeId = digest(canonical([scope, item.kind, item.field ?? null, item.id, semantic]));
    if (uniqueItems.has(changeId)) return;
    uniqueItems.add(changeId);
    if (!Object.hasOwn(seen, changeId) && item.direction !== "outgoing") changes.push({ kind: item.kind, changeId });
    seen[changeId] = Date.now();
    items.push({ ...item, changeId });
  };
  for (const entry of entries) {
    signal?.throwIfAborted();
    if ((entry.changes !== undefined && !Array.isArray(entry.changes)) ||
        (entry.changed_fields !== undefined && (!Array.isArray(entry.changed_fields) ||
          entry.changed_fields.some((field) => typeof field !== "string"))))
      throw failure("configuration_invalid", "The webhook contains an invalid field list.");
    const notifiedAt = timestamp(entry.time, 1000);
    const fields = Array.isArray(entry.changes) ? entry.changes :
      typeof entry.field === "string" ? [{ field: entry.field, value: entry.value }] : [];
    for (const change of fields) {
      if (!record(change) || typeof change.field !== "string")
        throw failure("configuration_invalid", "The webhook contains an invalid field change.");
      if (!["comments", "live_comments"].includes(change.field)) {
        unhandled.push({ field: change.field, notifiedAt });
        continue;
      }
      const value = change.value;
      const id = value?.id ?? value?.comment_id;
      if (!record(value) || !identifier(id))
        throw failure("configuration_invalid", "The webhook contains a comment without a valid ID.");
      capture({ kind: "comment", field: change.field, id, notifiedAt,
        // Notification time is not the comment's creation time.
        createdAt: null, mediaId: identifier(value.media?.id) ? value.media.id : null,
        parentId: identifier(value.parent_id) ? value.parent_id : null,
        from: record(value.from) ? value.from : null,
        text: typeof value.text === "string" ? value.text : null,
        direction: ownerIds.has(value.from?.id) ? "outgoing" : identifier(value.from?.id) ? "incoming" : "unknown",
      }, value);
    }
    if (entry.messaging !== undefined && !Array.isArray(entry.messaging))
      throw failure("configuration_invalid", "The webhook contains an invalid messaging list.");
    for (const event of entry.messaging ?? []) {
      if (!record(event)) throw failure("configuration_invalid", "The webhook contains an invalid messaging event.");
      if (!record(event.message)) {
        unhandled.push({ field: Object.keys(event).filter((key) => !["sender", "recipient", "timestamp"].includes(key)).join(","), notifiedAt });
        continue;
      }
      const message = event.message;
      if (!identifier(message.mid) || (!ownerIds.has(event.sender?.id) && !ownerIds.has(event.recipient?.id)))
        throw failure("authentication_required", "The webhook message does not match the configured account.");
      const direction = message.is_echo === true || ownerIds.has(event.sender?.id) ? "outgoing" : "incoming";
      capture({ kind: "direct-message", id: message.mid, notifiedAt, createdAt: timestamp(event.timestamp, 1),
        from: record(event.sender) ? event.sender : null, to: record(event.recipient) ? event.recipient : null,
        text: typeof message.text === "string" ? message.text : null,
        deleted: message.is_deleted === true, attachments: Array.isArray(message.attachments) ? message.attachments : [], direction,
      }, { sender: event.sender, recipient: event.recipient, timestamp: event.timestamp, message });
    }
    for (const field of entry.changed_fields ?? []) unhandled.push({ field, notifiedAt });
  }
  const retained = Object.entries(seen).sort((a, b) => b[1] - a[1]);
  const deduplicationComplete = retained.length <= maximumSeen && previous?.deduplicationComplete !== false;
  const data = {
    schemaVersion: 1, platform: "instagram", source: "instagram-webhook", collectedAt,
    receivedAt: new Date(Date.parse(input.receivedAt)).toISOString(), account: { id: config.accountId },
    delivery: { id: digest(rawBody), signatureVerified: true, matchingEntries: entries.length },
    coverage: { complete: unhandled.length === 0, unhandled },
    deduplication: { complete: deduplicationComplete, retentionDays, maximumSeen },
    items,
    // Keep authenticated account entries for new payload variants and diagnostics.
    notifications: entries,
    unreadMessages: { status: "unavailable", value: null,
      reason: "Webhook arrivals and read receipts do not provide account-wide unread inbox state." },
  };
  if (Buffer.byteLength(JSON.stringify(data)) > 500000)
    throw failure("configuration_invalid", "The normalized webhook result exceeds the size limit.");
  const events = changes.length ? [{ name: "social.webhook.received", payload: {
    platform: "instagram", accountId: config.accountId, collectedAt,
    comments: changes.filter((item) => item.kind === "comment").length,
    directMessages: changes.filter((item) => item.kind === "direct-message").length,
    changeIds: changes.slice(0, 32).map((item) => item.changeId), additionalChanges: Math.max(0, changes.length - 32),
  } }] : [];
  return { data, state: { webhook: { scope, seen: Object.fromEntries(retained.slice(0, maximumSeen)), deduplicationComplete } }, events };
}
