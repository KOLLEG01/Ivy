import assert from "node:assert/strict";
import {
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import test from "node:test";
import instagram from "../docs/examples/data-collector/social/instagram.mjs";
import tiktok from "../docs/examples/data-collector/social/tiktok.mjs";
import youtube from "../docs/examples/data-collector/social/youtube.mjs";

const now = Date.UTC(2026, 8, 26, 12);
const day = 86400000;
const stamp = (daysAgo) => new Date(now - daysAgo * day).toISOString();
const igConfig = {
  accountId: "123",
  apiVersion: "v25.0",
  accessTokenSecret: "token",
};
const tokenSecrets = { token: "fixture-access-token" };
const igMedia = (id, daysAgo, extra = {}) => ({
  id,
  timestamp: stamp(daysAgo),
  media_type: "IMAGE",
  like_count: 0,
  comments_count: 3,
  ...extra,
});
const ttVideo = (id, daysAgo, extra = {}) => ({
  id,
  create_time: (now - daysAgo * day) / 1000,
  view_count: 0,
  like_count: 2,
  comment_count: 0,
  ...extra,
});
const ttOk = (data) => ({ data, error: { code: "ok", message: "" } });
const ytChannel = (stats = {}) => ({
  id: "owner",
  snippet: { title: "Fixture channel" },
  statistics: {
    subscriberCount: "1200",
    hiddenSubscriberCount: false,
    ...stats,
  },
  contentDetails: { relatedPlaylists: { uploads: "uploads" } },
});
const ytVideo = (id, daysAgo, extra = {}) => ({
  id,
  snippet: {
    publishedAt: stamp(daysAgo),
    channelId: "owner",
    title: "Fixture video",
  },
  statistics: { viewCount: "12", likeCount: "0", commentCount: "3" },
  status: { privacyStatus: "public" },
  ...extra,
});

function fixture(t, entries) {
  t.mock.timers.enable({ apis: ["Date"], now });
  let calls = 0;
  t.mock.method(globalThis, "fetch", async (url, options) => {
    assert.ok(calls < entries.length, "Unexpected API request");
    const expected = entries[calls++];
    const actual = new URL(url);
    assert.equal(actual.host, expected.host ?? actual.host);
    assert.equal(actual.pathname, expected.path);
    assert.equal(options.method ?? "GET", expected.method ?? "GET");
    assert.equal(options.redirect, "error");
    assert.equal(
      actual.searchParams.has("access_token"),
      expected.tokenQuery ?? false,
    );
    expected.check?.(actual, options);
    if (expected.error) throw expected.error;
    return new Response(JSON.stringify(expected.body), {
      status: expected.status ?? 200,
    });
  });
  t.after(() =>
    assert.equal(calls, entries.length, "Expected all fixture API requests"),
  );
}

async function privateDirectory(t) {
  const parent = resolve(process.env.IVY_TEST_TEMP ?? tmpdir());
  const directory = await mkdtemp(join(parent, "social-auth-"));
  t.mock.method(process, "cwd", () => directory);
  t.after(async () => {
    assert.equal(dirname(directory), parent);
    await rm(directory, { recursive: true, force: true });
  });
  return directory;
}

async function authCache(directory, platform) {
  const path = join(directory, ".auth", `${platform}.json`);
  if (process.platform !== "win32") {
    assert.equal((await stat(path)).mode & 0o777, 0o600);
    assert.equal((await stat(join(directory, ".auth"))).mode & 0o777, 0o700);
  }
  assert.deepEqual(await readdir(join(directory, ".auth")), [
    `${platform}.json`,
  ]);
  return JSON.parse(await readFile(path, "utf8"));
}

test("Instagram walks unordered media pages, deduplicates, and preserves zeros and unavailable insights", async (t) => {
  fixture(t, [
    {
      path: "/v25.0/123",
      host: "graph.instagram.com",
      body: {
        id: "app-scoped",
        user_id: "123",
        username: "fixture",
        followers_count: 0,
      },
    },
    {
      path: "/v25.0/123/media",
      body: {
        data: [igMedia("1", 31)],
        paging: {
          next: "https://untrusted.invalid/?access_token=never-forward",
          cursors: { after: "next-cursor" },
        },
      },
    },
    {
      path: "/v25.0/123/media",
      check: (url) =>
        assert.equal(url.searchParams.get("after"), "next-cursor"),
      body: {
        data: [
          igMedia("2", 2),
          igMedia("3", 30),
          igMedia("2", 2),
          igMedia("4", -1),
        ],
      },
    },
    {
      path: "/v25.0/2/insights",
      body: { data: [{ name: "views", values: [{ value: 0 }] }] },
    },
    {
      path: "/v25.0/3/insights",
      status: 400,
      body: { error: { code: 100, message: "unsupported metric" } },
    },
  ]);
  const { data } = await instagram({ config: igConfig, secrets: tokenSecrets });
  assert.equal(data.account.followers.value, 0);
  assert.equal(data.posts.complete, true);
  assert.deepEqual(
    data.posts.items.map((item) => item.id),
    ["2", "3"],
  );
  assert.equal(data.posts.items[0].views.value, 0);
  assert.equal(data.posts.items[0].likes.value, 0);
  assert.equal(data.posts.items[1].views.status, "unavailable");
  assert.equal(data.posts.items[1].views.value, null);
  assert.match(data.posts.items[1].views.reason, /Graph code 100/);
  assert.equal(data.unreadMessages.value, null);
});

test("Instagram reports a capped traversal and omitted metrics without inventing zero", async (t) => {
  fixture(t, [
    { path: "/v25.0/123", host: "graph.facebook.com", body: { id: "123" } },
    {
      path: "/v25.0/123/media",
      body: {
        data: [igMedia("1", 1, { like_count: undefined })],
        paging: { next: "unused", cursors: { after: "cursor" } },
      },
    },
  ]);
  const { data } = await instagram({
    config: {
      ...igConfig,
      login: "facebook",
      maxPages: 1,
      collectInsights: false,
    },
    secrets: tokenSecrets,
  });
  assert.equal(data.posts.complete, false);
  assert.match(data.posts.reason, /maxPages/);
  assert.equal(data.account.followers.status, "unavailable");
  assert.equal(data.posts.items[0].likes.value, null);
  assert.equal(data.posts.items[0].views.status, "unavailable");
});

test("Instagram authentication failure is a failed run, not an unavailable view count", async (t) => {
  fixture(t, [
    { path: "/v25.0/123", body: { user_id: "123", followers_count: 7 } },
    { path: "/v25.0/123/media", body: { data: [igMedia("1", 1)] } },
    {
      path: "/v25.0/1/insights",
      status: 400,
      body: {
        error: { code: 190, message: "fixture-access-token is expired" },
      },
    },
  ]);
  await assert.rejects(
    instagram({ config: igConfig, secrets: tokenSecrets }),
    (error) =>
      /code 190/.test(error.message) &&
      !error.message.includes(tokenSecrets.token),
  );
});

test("Instagram throttling fails the run even with a generic metric error code", async (t) => {
  fixture(t, [
    { path: "/v25.0/123", body: { user_id: "123", followers_count: 7 } },
    { path: "/v25.0/123/media", body: { data: [igMedia("1", 1)] } },
    { path: "/v25.0/1/insights", status: 429, body: { error: { code: 100 } } },
  ]);
  await assert.rejects(
    instagram({ config: igConfig, secrets: tokenSecrets }),
    /HTTP 429/,
  );
});

test("Instagram persists a refreshed long-lived token before a failed collection and invalidates replaced credentials", async (t) => {
  const directory = await privateDirectory(t);
  fixture(t, [
    {
      path: "/refresh_access_token",
      host: "graph.instagram.com",
      tokenQuery: true,
      check: (url) => {
        assert.equal(url.searchParams.get("grant_type"), "ig_refresh_token");
        assert.equal(url.searchParams.get("access_token"), tokenSecrets.token);
      },
      body: {
        access_token: "ig-rotated",
        token_type: "bearer",
        expires_in: 5184000,
      },
    },
    {
      path: "/v25.0/123",
      check: (url, options) => {
        assert.match(url.searchParams.get("fields"), /user_id/);
        assert.equal(options.headers.Authorization, "Bearer ig-rotated");
      },
      status: 503,
      body: { error: { code: 2, message: "ig-rotated" } },
    },
    {
      path: "/v25.0/123",
      check: (_, options) =>
        assert.equal(options.headers.Authorization, "Bearer ig-rotated"),
      body: {
        id: "different-app-scoped-id",
        user_id: "123",
        followers_count: 5,
      },
    },
    { path: "/v25.0/123/media", body: { data: [] } },
    {
      path: "/v25.0/123",
      check: (_, options) =>
        assert.equal(options.headers.Authorization, "Bearer newly-provisioned"),
      body: { user_id: "123" },
    },
    { path: "/v25.0/123/media", body: { data: [] } },
  ]);
  const config = {
    ...igConfig,
    authMode: "long_lived",
    accessTokenIssuedAt: stamp(54),
  };
  await assert.rejects(instagram({ config, secrets: tokenSecrets }), {
    code: "provider_unavailable",
  });
  assert.equal(
    (await authCache(directory, "instagram")).accessToken,
    "ig-rotated",
  );
  const result = await instagram({ config, secrets: tokenSecrets });
  assert.equal(result.data.account.id, "123");
  assert.equal(result.state, undefined);
  assert.equal(JSON.stringify(result).includes("ig-rotated"), false);
  await instagram({
    config: { ...config, accessTokenIssuedAt: stamp(2) },
    secrets: { token: "newly-provisioned" },
  });
  assert.equal(
    (await authCache(directory, "instagram")).accessToken,
    "newly-provisioned",
  );
});

test("Instagram exchanges a short-lived token once and requires a new grant after cached expiry", async (t) => {
  const directory = await privateDirectory(t);
  fixture(t, [
    {
      path: "/access_token",
      tokenQuery: true,
      check: (url) => {
        assert.equal(url.searchParams.get("grant_type"), "ig_exchange_token");
        assert.equal(
          url.searchParams.get("client_secret"),
          "fixture-app-secret",
        );
      },
      body: {
        access_token: "ig-long-lived",
        token_type: "bearer",
        expires_in: 5184000,
      },
    },
    { path: "/v25.0/123", body: { user_id: "123" } },
    { path: "/v25.0/123/media", body: { data: [] } },
    { path: "/v25.0/123", body: { user_id: "123" } },
    { path: "/v25.0/123/media", body: { data: [] } },
  ]);
  const config = {
    ...igConfig,
    authMode: "short_lived",
    clientSecretSecret: "app",
  };
  const secrets = { ...tokenSecrets, app: "fixture-app-secret" };
  await instagram({ config, secrets });
  await instagram({ config, secrets });
  assert.equal(
    (await authCache(directory, "instagram")).accessToken,
    "ig-long-lived",
  );
  t.mock.timers.setTime(now + 61 * day);
  await assert.rejects(instagram({ config, secrets }), {
    code: "authentication_required",
  });
});

test("Instagram refuses refresh modes for Facebook Login instead of applying the wrong token protocol", async (t) => {
  const noFetch = t.mock.method(globalThis, "fetch", () => {
    throw new Error("Must not request");
  });
  await assert.rejects(
    instagram({
      config: { ...igConfig, login: "facebook", authMode: "long_lived" },
      secrets: tokenSecrets,
    }),
    { code: "configuration_invalid" },
  );
  assert.equal(noFetch.mock.callCount(), 0);
});

test("TikTok follows cursors, includes the exact date boundary, and stops after the documented cutoff", async (t) => {
  fixture(t, [
    {
      path: "/v2/user/info/",
      host: "open.tiktokapis.com",
      check: (url) =>
        assert.match(url.searchParams.get("fields"), /follower_count/),
      body: ttOk({ user: { open_id: "account", follower_count: 4 } }),
    },
    {
      path: "/v2/video/list/",
      method: "POST",
      check: (_, options) =>
        assert.deepEqual(JSON.parse(options.body), { max_count: 20 }),
      body: ttOk({
        videos: [ttVideo("a", 1)],
        has_more: true,
        cursor: now - day,
      }),
    },
    {
      path: "/v2/video/list/",
      method: "POST",
      check: (_, options) =>
        assert.equal(JSON.parse(options.body).cursor, now - day),
      body: ttOk({
        videos: [ttVideo("b", 30), ttVideo("old", 31)],
        has_more: true,
        cursor: now - 31 * day,
      }),
    },
  ]);
  const { data } = await tiktok({
    config: { accessTokenSecret: "token", openId: "account" },
    secrets: tokenSecrets,
  });
  assert.deepEqual(
    data.posts.items.map((item) => item.id),
    ["a", "b"],
  );
  assert.equal(data.posts.complete, true);
  assert.equal(data.posts.items[0].views.value, 0);
  assert.equal(data.unreadMessages.status, "unavailable");
});

test("TikTok rejects a successful HTTP response containing an API error without exposing its text", async (t) => {
  fixture(t, [
    {
      path: "/v2/user/info/",
      body: {
        error: {
          code: "access_token_invalid",
          message: "fixture-access-token",
        },
      },
    },
  ]);
  await assert.rejects(
    tiktok({ config: { accessTokenSecret: "token" }, secrets: tokenSecrets }),
    (error) =>
      /TikTok API error/.test(error.message) &&
      !error.message.includes(tokenSecrets.token),
  );
});

test("TikTok detects a repeated cursor rather than looping indefinitely", async (t) => {
  fixture(t, [
    { path: "/v2/user/info/", body: ttOk({ user: { open_id: "account" } }) },
    {
      path: "/v2/video/list/",
      method: "POST",
      body: ttOk({ videos: [], has_more: true, cursor: now }),
    },
    {
      path: "/v2/video/list/",
      method: "POST",
      body: ttOk({ videos: [], has_more: true, cursor: now }),
    },
  ]);
  await assert.rejects(
    tiktok({ config: { accessTokenSecret: "token" }, secrets: tokenSecrets }),
    /repeated pagination cursor/,
  );
});

test("TikTok saves refresh rotation before collection failure, refreshes with the rotated token, and honors secret replacement", async (t) => {
  const directory = await privateDirectory(t);
  const oauth = (access, refresh) => ({
    access_token: access,
    refresh_token: refresh,
    open_id: "account",
    token_type: "Bearer",
    expires_in: 86400,
    refresh_expires_in: 31536000,
  });
  const refreshRequest = (expectedRefresh, body) => ({
    path: "/v2/oauth/token/",
    host: "open.tiktokapis.com",
    method: "POST",
    check: (_, options) => {
      const form = new URLSearchParams(options.body);
      assert.equal(form.get("grant_type"), "refresh_token");
      assert.equal(form.get("refresh_token"), expectedRefresh);
      assert.equal(form.get("client_key"), "fixture-client");
      assert.equal(form.get("client_secret"), "secret&with=encoding");
    },
    body,
  });
  const profile = (access) => ({
    path: "/v2/user/info/",
    check: (_, options) =>
      assert.equal(options.headers.Authorization, `Bearer ${access}`),
    body: ttOk({ user: { open_id: "account", follower_count: 2 } }),
  });
  const videos = {
    path: "/v2/video/list/",
    method: "POST",
    body: ttOk({ videos: [], has_more: false }),
  };
  fixture(t, [
    refreshRequest("initial-refresh", oauth("access-a", "refresh-a")),
    {
      path: "/v2/user/info/",
      status: 503,
      body: { error: { code: "internal_error" } },
    },
    profile("access-a"),
    videos,
    refreshRequest("refresh-a", oauth("access-b", "refresh-b")),
    profile("access-b"),
    videos,
    refreshRequest("replacement-refresh", oauth("access-c", "refresh-c")),
    profile("access-c"),
    videos,
    {
      ...refreshRequest("refresh-c", {
        error: "invalid_grant",
        error_description: "refresh-c",
      }),
      status: 400,
    },
  ]);
  const config = {
    authMode: "refresh",
    openId: "account",
    clientKeySecret: "client",
    clientSecretSecret: "app",
    refreshTokenSecret: "refresh",
  };
  const secrets = {
    client: "fixture-client",
    app: "secret&with=encoding",
    refresh: "initial-refresh",
  };
  await assert.rejects(tiktok({ config, secrets }), {
    code: "provider_unavailable",
  });
  assert.equal(
    (await authCache(directory, "tiktok")).refreshToken,
    "refresh-a",
  );
  const result = await tiktok({ config, secrets });
  assert.equal(result.state, undefined);
  assert.equal(JSON.stringify(result).includes("access-a"), false);
  assert.equal(JSON.stringify(result).includes("refresh-a"), false);
  t.mock.timers.setTime(now + day);
  await tiktok({ config, secrets });
  assert.equal(
    (await authCache(directory, "tiktok")).refreshToken,
    "refresh-b",
  );
  const replacements = { ...secrets, refresh: "replacement-refresh" };
  await tiktok({ config, secrets: replacements });
  const persisted = await authCache(directory, "tiktok");
  assert.equal(persisted.refreshToken, "refresh-c");
  t.mock.timers.setTime(now + 2 * day);
  await assert.rejects(
    tiktok({ config, secrets: replacements }),
    (error) =>
      error.code === "authentication_required" &&
      !error.message.includes("refresh-c"),
  );
  assert.deepEqual(await authCache(directory, "tiktok"), persisted);
});

test("TikTok refresh ignores unused access secrets and retains the rotated refresh grant", async (t) => {
  const directory = await privateDirectory(t);
  const refresh = (expected, access, rotated) => ({
    path: "/v2/oauth/token/",
    host: "open.tiktokapis.com",
    method: "POST",
    check: (_, options) =>
      assert.equal(
        new URLSearchParams(options.body).get("refresh_token"),
        expected,
      ),
    body: {
      access_token: access,
      refresh_token: rotated,
      open_id: "account",
      token_type: "Bearer",
      expires_in: 86400,
      refresh_expires_in: 31536000,
    },
  });
  const profile = (access) => ({
    path: "/v2/user/info/",
    check: (_, options) =>
      assert.equal(options.headers.Authorization, `Bearer ${access}`),
    body: ttOk({ user: { open_id: "account", follower_count: 2 } }),
  });
  const videos = {
    path: "/v2/video/list/",
    method: "POST",
    body: ttOk({ videos: [], has_more: false }),
  };
  const config = {
    authMode: "refresh",
    openId: "account",
    clientKeySecret: "client",
    clientSecretSecret: "app",
    refreshTokenSecret: "refresh",
  };
  const secrets = {
    client: "fixture-client",
    app: "fixture-secret",
    refresh: "initial-refresh",
  };
  const withUnused = { ...config, accessTokenSecret: "unused" };
  const variants = [
    { config: withUnused, secrets },
    { config: withUnused, secrets: { ...secrets, unused: "unused-access-a" } },
    { config: withUnused, secrets: { ...secrets, unused: "unused-access-b" } },
    { config: withUnused, secrets },
    { config: { ...config, accessTokenSecret: "replaced-reference" }, secrets },
    { config, secrets },
  ];
  fixture(t, [
    refresh("initial-refresh", "cached-access", "rotated-refresh"),
    ...variants.flatMap(() => [profile("cached-access"), videos]),
    refresh("rotated-refresh", "renewed-access", "latest-refresh"),
    profile("renewed-access"),
    videos,
  ]);
  let saved;
  for (const context of variants) {
    assert.equal((await tiktok(context)).data.account.id, "account");
    const cached = await authCache(directory, "tiktok");
    assert.equal(cached.refreshToken, "rotated-refresh");
    if (saved) assert.deepEqual(cached, saved);
    saved = cached;
  }
  t.mock.timers.setTime(now + day);
  await tiktok({ config, secrets });
  assert.equal(
    (await authCache(directory, "tiktok")).refreshToken,
    "latest-refresh",
  );
});

test("TikTok uses cached access after refresh expiry, including its refresh margin, until access itself expires", async (t) => {
  const directory = await privateDirectory(t);
  const profile = {
    path: "/v2/user/info/",
    check: (_, options) =>
      assert.equal(options.headers.Authorization, "Bearer cached-access"),
    body: ttOk({ user: { open_id: "account", follower_count: 2 } }),
  };
  const videos = {
    path: "/v2/video/list/",
    method: "POST",
    body: ttOk({ videos: [], has_more: false }),
  };
  fixture(t, [
    {
      path: "/v2/oauth/token/",
      method: "POST",
      body: {
        access_token: "cached-access",
        refresh_token: "rotated-refresh",
        open_id: "account",
        token_type: "Bearer",
        expires_in: 86400,
        refresh_expires_in: 1,
      },
    },
    profile,
    videos,
    profile,
    videos,
    profile,
    videos,
  ]);
  const context = {
    config: {
      authMode: "refresh",
      openId: "account",
      clientKeySecret: "client",
      clientSecretSecret: "app",
      refreshTokenSecret: "refresh",
    },
    secrets: {
      client: "fixture-client",
      app: "fixture-secret",
      refresh: "initial-refresh",
    },
  };
  await tiktok(context);
  const cached = await authCache(directory, "tiktok");
  t.mock.timers.setTime(now + 1000);
  assert.equal((await tiktok(context)).data.account.id, "account");
  t.mock.timers.setTime(now + day - 299000);
  assert.equal((await tiktok(context)).data.account.id, "account");
  t.mock.timers.setTime(now + day);
  await assert.rejects(tiktok(context), { code: "authentication_required" });
  assert.deepEqual(await authCache(directory, "tiktok"), cached);
});

test("A corrupted TikTok rotation cache fails without reusing the originally provisioned refresh token", async (t) => {
  const directory = await privateDirectory(t);
  await mkdir(join(directory, ".auth"), { mode: 0o700 });
  await writeFile(join(directory, ".auth", "tiktok.json"), "{", {
    mode: 0o600,
  });
  const noFetch = t.mock.method(globalThis, "fetch", () => {
    throw new Error("Must not request");
  });
  await assert.rejects(
    tiktok({
      config: {
        authMode: "refresh",
        clientKeySecret: "client",
        clientSecretSecret: "app",
        refreshTokenSecret: "refresh",
      },
      secrets: { client: "client", app: "app", refresh: "initial" },
    }),
    { code: "configuration_invalid" },
  );
  assert.equal(noFetch.mock.callCount(), 0);
});

test("YouTube refreshes owner OAuth and finds recent publications beyond older upload entries", async (t) => {
  fixture(t, [
    {
      path: "/token",
      host: "oauth2.googleapis.com",
      method: "POST",
      check: (_, options) => {
        const form = new URLSearchParams(options.body);
        assert.equal(form.get("grant_type"), "refresh_token");
        assert.equal(form.get("client_secret"), "secret&with=encoding");
        assert.equal(form.get("refresh_token"), "fixture-refresh-token");
      },
      body: { access_token: "fresh-token" },
    },
    {
      path: "/youtube/v3/channels",
      host: "www.googleapis.com",
      check: (url, options) => {
        assert.equal(url.searchParams.get("mine"), "true");
        assert.equal(options.headers.Authorization, "Bearer fresh-token");
      },
      body: { items: [ytChannel()] },
    },
    {
      path: "/youtube/v3/playlistItems",
      body: {
        items: [{ contentDetails: { videoId: "old" } }],
        nextPageToken: "page-2",
      },
    },
    { path: "/youtube/v3/videos", body: { items: [ytVideo("old", 31)] } },
    {
      path: "/youtube/v3/playlistItems",
      check: (url) => assert.equal(url.searchParams.get("pageToken"), "page-2"),
      body: { items: [{ contentDetails: { videoId: "recent" } }] },
    },
    {
      path: "/youtube/v3/videos",
      body: {
        items: [
          ytVideo("recent", 1, {
            statistics: { viewCount: "0", likeCount: "2" },
          }),
        ],
      },
    },
  ]);
  const result = await youtube({
    config: {
      channelId: "owner",
      clientIdSecret: "client",
      clientSecretSecret: "secret",
      refreshTokenSecret: "refresh",
    },
    secrets: {
      client: "client",
      secret: "secret&with=encoding",
      refresh: "fixture-refresh-token",
    },
  });
  assert.equal(result.data.posts.complete, true);
  assert.deepEqual(
    result.data.posts.items.map((item) => item.id),
    ["recent"],
  );
  assert.equal(result.data.posts.items[0].views.value, 0);
  assert.equal(result.data.posts.items[0].comments.value, null);
  assert.match(
    result.data.account.subscribers.precision,
    /three significant figures/,
  );
  assert.equal(result.state, undefined);
  assert.equal(JSON.stringify(result).includes("fresh-token"), false);
  assert.equal(JSON.stringify(result).includes("fixture-refresh-token"), false);
});

test("YouTube reports hidden subscribers, omitted uploads and capped coverage explicitly", async (t) => {
  fixture(t, [
    {
      path: "/youtube/v3/channels",
      body: { items: [ytChannel({ hiddenSubscriberCount: true })] },
    },
    {
      path: "/youtube/v3/playlistItems",
      body: {
        items: [
          { contentDetails: { videoId: "a" } },
          { contentDetails: { videoId: "missing" } },
        ],
        nextPageToken: "page-2",
      },
    },
    {
      path: "/youtube/v3/videos",
      body: {
        items: [
          ytVideo("a", 1, { statistics: { viewCount: "9007199254740993" } }),
        ],
      },
    },
  ]);
  const { data } = await youtube({
    config: { accessTokenSecret: "token", maxPages: 1 },
    secrets: tokenSecrets,
  });
  assert.equal(data.account.subscribers.value, null);
  assert.match(data.account.subscribers.reason, /hidden/);
  assert.equal(data.posts.complete, false);
  assert.match(data.posts.reason, /maxPages/);
  assert.deepEqual(data.posts.unavailableIds, ["missing"]);
  assert.equal(data.posts.items[0].views.value, "9007199254740993");
  assert.equal(data.posts.items[0].views.representation, "decimal_string");
});

test("YouTube rejects an OAuth grant for a different channel before collecting posts", async (t) => {
  fixture(t, [
    { path: "/youtube/v3/channels", body: { items: [ytChannel()] } },
  ]);
  await assert.rejects(
    youtube({
      config: { channelId: "other", accessTokenSecret: "token" },
      secrets: tokenSecrets,
    }),
    /intended channel/,
  );
});

test("Collectors fail promptly on cancellation and missing secret references", async (t) => {
  const noFetch = t.mock.method(globalThis, "fetch", () => {
    throw new Error("Must not request");
  });
  const controller = new AbortController();
  controller.abort();
  for (const [collect, config] of [
    [instagram, igConfig],
    [tiktok, { accessTokenSecret: "token" }],
    [youtube, { accessTokenSecret: "token" }],
  ]) {
    await assert.rejects(
      collect({ config, secrets: {}, signal: controller.signal }),
      /secret.*missing/i,
    );
    await assert.rejects(
      collect({ config, secrets: tokenSecrets, signal: controller.signal }),
      { name: "AbortError" },
    );
  }
  assert.equal(noFetch.mock.callCount(), 0);
});

test("Account task templates stay disabled and reference external secret names", async () => {
  for (const platform of ["instagram", "tiktok", "youtube"]) {
    const task = JSON.parse(
      await readFile(
        new URL(
          `../docs/examples/data-collector/social/${platform}.task.json`,
          import.meta.url,
        ),
        "utf8",
      ),
    );
    assert.equal(task.enabled, false);
    assert.equal(task.script, `@file:${platform}.mjs`);
    assert.deepEqual(task.dependencies, {});
    assert.deepEqual(task.retention, {
      maximumCount: 1000,
      maximumAgeDays: 7,
      maximumBytes: 104857600,
    });
    for (const [name, value] of Object.entries(task.config)) {
      if (name.endsWith("Secret")) assert.ok(task.secretNames.includes(value));
    }
  }
});
