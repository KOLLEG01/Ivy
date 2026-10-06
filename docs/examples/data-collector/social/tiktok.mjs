import { createHash, randomUUID } from "node:crypto";
import { chmod, mkdir, open, readFile, rename, rm } from "node:fs/promises";
import { join } from "node:path";

const failure = (code, message) => Object.assign(new Error(message), { code });

async function tiktokToken(config, secrets, signal) {
  const secret = (name) => {
    const value = secrets[name];
    if (typeof value !== "string" || !value.trim())
      throw failure(
        "authentication_required",
        "A configured TikTok secret is missing.",
      );
    return value;
  };
  const mode =
    config.authMode ?? (config.refreshTokenSecret ? "refresh" : "token");
  if (mode === "token") return secret(config.accessTokenSecret);
  if (mode !== "refresh")
    throw failure(
      "configuration_invalid",
      "TikTok authMode must be token or refresh.",
    );
  const clientKey = secret(config.clientKeySecret);
  const clientSecret = secret(config.clientSecretSecret);
  const provisionedRefresh = secret(config.refreshTokenSecret);
  const fingerprint = createHash("sha256")
    .update(
      JSON.stringify({
        mode,
        openId: config.openId ?? null,
        clientKeySecret: config.clientKeySecret,
        clientKey,
        clientSecretSecret: config.clientSecretSecret,
        clientSecret,
        refreshTokenSecret: config.refreshTokenSecret,
        provisionedRefresh,
      }),
    )
    .digest("hex");
  const directory = join(process.cwd(), ".auth");
  const file = join(directory, "tiktok.json");
  let auth;
  try {
    const cached = JSON.parse(await readFile(file, "utf8"));
    if (cached.fingerprint === fingerprint) {
      if (
        typeof cached.accessToken !== "string" ||
        !cached.accessToken ||
        typeof cached.refreshToken !== "string" ||
        !cached.refreshToken ||
        typeof cached.openId !== "string" ||
        !cached.openId ||
        !Number.isSafeInteger(cached.expiresAt) ||
        !Number.isSafeInteger(cached.refreshExpiresAt)
      )
        throw new Error("Invalid cache");
      auth = cached;
    }
  } catch (error) {
    if (error.code !== "ENOENT")
      throw failure(
        "configuration_invalid",
        "The private TikTok authentication cache cannot be read. Restore the rotated token cache or provision a new grant.",
      );
  }
  async function save(record) {
    const temporary = join(directory, `.tiktok-${randomUUID()}.tmp`);
    let handle;
    try {
      await mkdir(directory, { recursive: true, mode: 0o700 });
      await chmod(directory, 0o700);
      handle = await open(temporary, "wx", 0o600);
      await handle.writeFile(JSON.stringify(record));
      await handle.sync();
      await handle.close();
      handle = undefined;
      await rename(temporary, file);
    } catch {
      throw failure(
        "configuration_invalid",
        "The private TikTok authentication cache cannot be saved.",
      );
    } finally {
      await handle?.close();
      await rm(temporary, { force: true });
    }
  }
  const checkedAt = Date.now();
  const refreshExpired = auth && auth.refreshExpiresAt <= checkedAt;
  if (refreshExpired && auth.expiresAt <= checkedAt)
    throw failure(
      "authentication_required",
      "The TikTok refresh token expired; a new authorization is required.",
    );
  // An expired refresh grant cannot renew, but its cached access token remains
  // usable until its own expiry, including the usual five-minute refresh margin.
  if (!auth || (!refreshExpired && auth.expiresAt - checkedAt <= 300000)) {
    signal?.throwIfAborted();
    let response;
    try {
      response = await fetch("https://open.tiktokapis.com/v2/oauth/token/", {
        method: "POST",
        headers: { "Content-Type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams({
          client_key: clientKey,
          client_secret: clientSecret,
          grant_type: "refresh_token",
          refresh_token: auth?.refreshToken ?? provisionedRefresh,
        }),
        signal,
        redirect: "error",
      });
    } catch {
      signal?.throwIfAborted();
      throw failure(
        "provider_unavailable",
        "TikTok token renewal failed before a response was received.",
      );
    }
    let body;
    try {
      body = await response.json();
    } catch {
      throw failure(
        "provider_unavailable",
        "TikTok token renewal returned invalid JSON.",
      );
    }
    if (!response.ok || body.error)
      throw failure(
        [400, 401, 403].includes(response.status) ||
          body.error === "invalid_grant"
          ? "authentication_required"
          : "provider_unavailable",
        "TikTok token renewal failed. Check the OAuth client and refresh grant.",
      );
    if (
      typeof body.access_token !== "string" ||
      !body.access_token ||
      typeof body.refresh_token !== "string" ||
      !body.refresh_token ||
      typeof body.open_id !== "string" ||
      !body.open_id ||
      typeof body.token_type !== "string" ||
      body.token_type.toLowerCase() !== "bearer" ||
      !Number.isSafeInteger(body.expires_in) ||
      body.expires_in <= 0 ||
      !Number.isSafeInteger(body.refresh_expires_in) ||
      body.refresh_expires_in <= 0 ||
      !Number.isSafeInteger(Date.now() + body.expires_in * 1000) ||
      !Number.isSafeInteger(Date.now() + body.refresh_expires_in * 1000)
    )
      throw failure(
        "provider_unavailable",
        "TikTok token renewal returned an invalid token response.",
      );
    auth = {
      fingerprint,
      accessToken: body.access_token,
      refreshToken: body.refresh_token,
      openId: body.open_id,
      expiresAt: Date.now() + body.expires_in * 1000,
      refreshExpiresAt: Date.now() + body.refresh_expires_in * 1000,
    };
    // TikTok may rotate its refresh token. Persist it before any data call can fail.
    await save(auth);
  }
  if (config.openId && config.openId !== auth.openId)
    throw failure(
      "authentication_required",
      "The TikTok refresh grant belongs to a different account.",
    );
  return auth.accessToken;
}

export default async function collectTikTok({ config, secrets, signal }) {
  const days = config.lookbackDays ?? 30;
  const maxPages = config.maxPages ?? 100;
  if (!Number.isInteger(days) || days < 1 || days > 365)
    throw failure("configuration_invalid", "lookbackDays must be 1..365.");
  if (!Number.isInteger(maxPages) || maxPages < 1 || maxPages > 1000)
    throw failure("configuration_invalid", "maxPages must be 1..1000.");
  const token = await tiktokToken(config, secrets, signal);
  const collectedAt = new Date().toISOString();
  const end = Date.parse(collectedAt);
  const start = end - days * 86400000;
  const unavailable = (reason, source) => ({
    status: "unavailable",
    value: null,
    reason,
    source,
  });
  const count = (value, source) =>
    Number.isSafeInteger(value) && value >= 0
      ? { status: "available", value, source }
      : unavailable(
          "The API omitted this count or returned a count outside JavaScript's safe integer range.",
          source,
        );
  async function request(path, fields, payload) {
    signal?.throwIfAborted();
    const url = new URL(`https://open.tiktokapis.com/v2/${path}/`);
    url.searchParams.set("fields", fields);
    let response;
    try {
      response = await fetch(url, {
        method: payload ? "POST" : "GET",
        headers: {
          Authorization: `Bearer ${token}`,
          ...(payload ? { "Content-Type": "application/json" } : {}),
        },
        ...(payload ? { body: JSON.stringify(payload) } : {}),
        signal,
        redirect: "error",
      });
    } catch (error) {
      signal?.throwIfAborted();
      throw failure(
        "provider_unavailable",
        "TikTok API request failed before a response was received.",
      );
    }
    let body;
    try {
      body = await response.json();
    } catch {
      throw failure(
        "provider_unavailable",
        `TikTok returned invalid JSON (HTTP ${response.status}).`,
      );
    }
    if (!response.ok || body.error?.code !== "ok") {
      // Do not retain provider error text: it may contain token or request values.
      const code =
        ["access_token_invalid", "scope_not_authorized"].includes(
          body.error?.code,
        ) || [401, 403].includes(response.status)
          ? "authentication_required"
          : "provider_unavailable";
      throw failure(
        code,
        `TikTok API error (HTTP ${response.status}). Check token, granted scopes, and rate limits.`,
      );
    }
    return body.data;
  }
  const profile = await request(
    "user/info",
    "open_id,display_name,follower_count",
  );
  const account = profile?.user;
  if (typeof account?.open_id !== "string" || !account.open_id)
    throw new Error("TikTok returned an invalid account profile.");
  if (config.openId && config.openId !== account.open_id)
    throw failure(
      "authentication_required",
      "The TikTok access token belongs to a different account.",
    );
  const posts = new Map();
  const seenCursors = new Set();
  let cursor;
  let complete = false;
  for (let page = 0; page < maxPages; page++) {
    const payload = { max_count: 20 };
    if (cursor !== undefined) payload.cursor = cursor;
    const result = await request(
      "video/list",
      "id,create_time,title,share_url,view_count,like_count,comment_count",
      payload,
    );
    if (!Array.isArray(result?.videos) || typeof result.has_more !== "boolean")
      throw new Error("TikTok returned an invalid video list.");
    let reachedCutoff = false;
    for (const video of result.videos) {
      const published = video.create_time * 1000;
      if (
        typeof video.id !== "string" ||
        !video.id ||
        !Number.isSafeInteger(video.create_time) ||
        !Number.isFinite(published)
      ) {
        throw new Error(
          "TikTok returned a video without a valid ID or creation time.",
        );
      }
      if (published < start) {
        reachedCutoff = true;
        continue;
      }
      if (published > end || posts.has(video.id)) continue;
      posts.set(video.id, {
        id: video.id,
        publishedAt: new Date(published).toISOString(),
        title: typeof video.title === "string" ? video.title : null,
        url: typeof video.share_url === "string" ? video.share_url : null,
        views: count(video.view_count, "video.view_count"),
        likes: count(video.like_count, "video.like_count"),
        comments: count(video.comment_count, "video.comment_count"),
      });
    }
    // The Display API documents descending create_time order.
    if (!result.has_more || reachedCutoff) {
      complete = true;
      break;
    }
    if (
      !Number.isSafeInteger(result.cursor) ||
      result.cursor < 0 ||
      seenCursors.has(result.cursor)
    )
      throw new Error(
        "TikTok returned an invalid or repeated pagination cursor.",
      );
    seenCursors.add(result.cursor);
    cursor = result.cursor;
  }
  return {
    data: {
      platform: "tiktok",
      collectedAt,
      account: {
        id: account.open_id,
        label: config.accountLabel ?? null,
        displayName: account.display_name ?? null,
        followers: count(account.follower_count, "user.follower_count"),
      },
      unreadMessages: unavailable(
        "TikTok Display API exposes profile information and public videos, not direct messages or unread inbox counts. Separate Business Messaging access requires its own approved business grant and is not included in this Display API collector.",
        "TikTok Display API",
      ),
      window: {
        days,
        start: new Date(start).toISOString(),
        end: collectedAt,
        counts: "lifetime totals for posts published in the window",
      },
      posts: {
        complete,
        reason: complete
          ? null
          : "Pagination reached maxPages before the date cutoff; recent public videos may be missing.",
        visibility: "public videos exposed by Display API",
        items: [...posts.values()].sort((a, b) =>
          b.publishedAt.localeCompare(a.publishedAt),
        ),
      },
    },
  };
}
