import { createHash, randomUUID } from "node:crypto";
import { chmod, mkdir, open, readFile, rename, rm } from "node:fs/promises";
import { join } from "node:path";

const failure = (code, message) => Object.assign(new Error(message), { code });

async function instagramToken(config, secrets, signal) {
  const secret = (name) => {
    const value = secrets[name];
    if (typeof value !== "string" || !value.trim())
      throw failure(
        "authentication_required",
        "A configured Instagram secret is missing.",
      );
    return value;
  };
  const supplied = secret(config.accessTokenSecret);
  const mode = config.authMode ?? "token";
  if (mode === "token") return supplied;
  if (
    !["long_lived", "short_lived"].includes(mode) ||
    (config.login ?? "instagram") !== "instagram"
  )
    throw failure(
      "configuration_invalid",
      "Instagram refresh requires Instagram Login and authMode long_lived or short_lived.",
    );
  const appSecret =
    mode === "short_lived" ? secret(config.clientSecretSecret) : null;
  const fingerprint = createHash("sha256")
    .update(
      JSON.stringify({
        accountId: config.accountId,
        login: config.login ?? "instagram",
        mode,
        accessTokenSecret: config.accessTokenSecret,
        supplied,
        clientSecretSecret: config.clientSecretSecret ?? null,
        appSecret,
        accessTokenIssuedAt: config.accessTokenIssuedAt ?? null,
      }),
    )
    .digest("hex");
  const directory = join(process.cwd(), ".auth");
  const file = join(directory, "instagram.json");
  let auth;
  try {
    const cached = JSON.parse(await readFile(file, "utf8"));
    if (cached.fingerprint === fingerprint) {
      if (
        typeof cached.accessToken !== "string" ||
        !cached.accessToken ||
        !Number.isSafeInteger(cached.issuedAt) ||
        !Number.isSafeInteger(cached.expiresAt) ||
        cached.expiresAt <= cached.issuedAt
      )
        throw new Error("Invalid cache");
      auth = cached;
    }
  } catch (error) {
    if (error.code !== "ENOENT")
      throw failure(
        "configuration_invalid",
        "The private Instagram authentication cache cannot be read. Restore it or provision a new token.",
      );
  }
  async function save(record) {
    const temporary = join(directory, `.instagram-${randomUUID()}.tmp`);
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
        "The private Instagram authentication cache cannot be saved.",
      );
    } finally {
      await handle?.close();
      await rm(temporary, { force: true });
    }
  }
  async function renew(path, parameters) {
    signal?.throwIfAborted();
    const url = new URL(`https://graph.instagram.com/${path}`);
    for (const [key, value] of Object.entries(parameters))
      url.searchParams.set(key, value);
    let response;
    try {
      response = await fetch(url, { signal, redirect: "error" });
    } catch {
      signal?.throwIfAborted();
      throw failure(
        "provider_unavailable",
        "Instagram token renewal failed before a response was received.",
      );
    }
    let body;
    try {
      body = await response.json();
    } catch {
      throw failure(
        "provider_unavailable",
        "Instagram token renewal returned invalid JSON.",
      );
    }
    if (!response.ok || body.error)
      throw failure(
        body.error?.code === 190 || [400, 401, 403].includes(response.status)
          ? "authentication_required"
          : "provider_unavailable",
        "Instagram token renewal failed. Provision a valid token with the required grant.",
      );
    if (
      typeof body.access_token !== "string" ||
      !body.access_token ||
      !Number.isSafeInteger(body.expires_in) ||
      body.expires_in <= 0 ||
      typeof body.token_type !== "string" ||
      body.token_type.toLowerCase() !== "bearer" ||
      !Number.isSafeInteger(Date.now() + body.expires_in * 1000)
    )
      throw failure(
        "provider_unavailable",
        "Instagram token renewal returned an invalid token response.",
      );
    const issuedAt = Date.now();
    const record = {
      fingerprint,
      accessToken: body.access_token,
      issuedAt,
      expiresAt: issuedAt + body.expires_in * 1000,
    };
    // Save the replacement before profile/media requests, even if those requests fail.
    await save(record);
    return record;
  }
  if (!auth) {
    if (mode === "short_lived") {
      auth = await renew("access_token", {
        grant_type: "ig_exchange_token",
        client_secret: appSecret,
        access_token: supplied,
      });
    } else {
      const issuedAt = Date.parse(config.accessTokenIssuedAt);
      if (!Number.isSafeInteger(issuedAt) || issuedAt > Date.now())
        throw failure(
          "configuration_invalid",
          "Set accessTokenIssuedAt to the actual long-lived token issuance time.",
        );
      auth = {
        fingerprint,
        accessToken: supplied,
        issuedAt,
        expiresAt: issuedAt + 60 * 86400000,
      };
      if (auth.expiresAt > Date.now()) await save(auth);
    }
  }
  if (auth.expiresAt <= Date.now())
    throw failure(
      "authentication_required",
      "The Instagram long-lived token has expired; a new authorization is required.",
    );
  // Meta permits refresh only after 24 hours. Refresh during the final seven days.
  if (
    auth.expiresAt - Date.now() <= 7 * 86400000 &&
    Date.now() - auth.issuedAt >= 86400000
  )
    auth = await renew("refresh_access_token", {
      grant_type: "ig_refresh_token",
      access_token: auth.accessToken,
    });
  return auth.accessToken;
}

export default async function collectInstagram({ config, secrets, signal }) {
  const version = config.apiVersion;
  if (typeof version !== "string" || !/^v\d+\.0$/.test(version)) {
    throw failure(
      "configuration_invalid",
      "Set apiVersion to a supported Meta Graph API version, for example v26.0.",
    );
  }
  const accountId = config.accountId;
  if (typeof accountId !== "string" || !/^\d+$/.test(accountId)) {
    throw failure(
      "configuration_invalid",
      "accountId must be the authorized Instagram professional account ID.",
    );
  }
  const login = config.login ?? "instagram";
  if (!["instagram", "facebook"].includes(login))
    throw failure(
      "configuration_invalid",
      "login must be instagram or facebook.",
    );
  const days = config.lookbackDays ?? 30;
  const maxPages = config.maxPages ?? 100;
  if (!Number.isInteger(days) || days < 1 || days > 365)
    throw failure("configuration_invalid", "lookbackDays must be 1..365.");
  if (!Number.isInteger(maxPages) || maxPages < 1 || maxPages > 1000)
    throw failure("configuration_invalid", "maxPages must be 1..1000.");
  const token = await instagramToken(config, secrets, signal);
  const collectedAt = new Date().toISOString();
  const end = Date.parse(collectedAt);
  const start = end - days * 86400000;
  const host =
    login === "instagram" ? "graph.instagram.com" : "graph.facebook.com";
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
  async function get(path, params) {
    signal?.throwIfAborted();
    const url = new URL(`https://${host}/${version}/${path}`);
    for (const [key, value] of Object.entries(params))
      url.searchParams.set(key, String(value));
    let response;
    try {
      response = await fetch(url, {
        headers: { Authorization: `Bearer ${token}` },
        signal,
        redirect: "error",
      });
    } catch (error) {
      signal?.throwIfAborted();
      throw failure(
        "provider_unavailable",
        "Instagram API request failed before a response was received.",
      );
    }
    let body;
    try {
      body = await response.json();
    } catch {
      throw failure(
        "provider_unavailable",
        `Instagram returned invalid JSON (HTTP ${response.status}).`,
      );
    }
    if (!response.ok || body.error) {
      const code = body.error?.code;
      const error = failure(
        code === 190 || [401, 403].includes(response.status)
          ? "authentication_required"
          : "provider_unavailable",
        `Instagram API error (HTTP ${response.status}, code ${Number.isInteger(code) ? code : "unknown"}).`,
      );
      error.apiCode = code;
      error.httpStatus = response.status;
      throw error;
    }
    return body;
  }
  const account = await get(accountId, {
    fields: `${login === "instagram" ? "user_id" : "id"},username,followers_count`,
  });
  const returnedAccountId =
    login === "instagram" ? account.user_id : account.id;
  if (returnedAccountId !== accountId)
    throw failure(
      "authentication_required",
      "Instagram returned an unexpected account ID.",
    );
  const posts = new Map();
  const seenCursors = new Set();
  let after;
  let complete = false;
  for (let page = 0; page < maxPages; page++) {
    const params = {
      fields:
        "id,caption,media_type,media_product_type,permalink,timestamp,like_count,comments_count",
      limit: 100,
    };
    if (after) params.after = after;
    const result = await get(`${accountId}/media`, params);
    if (!Array.isArray(result.data))
      throw new Error("Instagram returned an invalid media list.");
    // Meta does not support ordering this edge; an old post is not an end cursor.
    for (const media of result.data) {
      const published = Date.parse(media.timestamp);
      if (
        typeof media.id !== "string" ||
        !/^\d+$/.test(media.id) ||
        !Number.isFinite(published)
      ) {
        throw new Error(
          "Instagram returned a media item without a valid ID or timestamp.",
        );
      }
      if (published < start || published > end || posts.has(media.id)) continue;
      let views = unavailable(
        "Insights collection is disabled in config.collectInsights.",
        "media.insights.views",
      );
      if (config.collectInsights !== false) {
        try {
          const insights = await get(`${media.id}/insights`, {
            metric: "views",
          });
          if (!Array.isArray(insights.data))
            throw new Error("Instagram returned invalid media insights.");
          const metric = insights.data.find((item) => item.name === "views");
          const value =
            metric?.total_value?.value ??
            (metric?.values?.length === 1 ? metric.values[0].value : undefined);
          views = count(value, "media.insights.views");
        } catch (error) {
          if (
            ![100, 10, 200].includes(error.apiCode) ||
            ![400, 403].includes(error.httpStatus)
          )
            throw error;
          views = unavailable(
            `The API rejected views for this media or permission grant (Graph code ${error.apiCode}).`,
            "media.insights.views",
          );
        }
      }
      posts.set(media.id, {
        id: media.id,
        publishedAt: new Date(published).toISOString(),
        caption: typeof media.caption === "string" ? media.caption : null,
        mediaType: media.media_type ?? null,
        mediaProductType: media.media_product_type ?? null,
        url: typeof media.permalink === "string" ? media.permalink : null,
        views,
        likes: count(media.like_count, "media.like_count"),
        comments: count(media.comments_count, "media.comments_count"),
      });
    }
    if (!result.paging?.next) {
      complete = true;
      break;
    }
    const next = result.paging?.cursors?.after;
    if (typeof next !== "string" || !next || seenCursors.has(next))
      throw new Error(
        "Instagram returned an invalid or repeated pagination cursor.",
      );
    seenCursors.add(next);
    after = next;
  }
  return {
    data: {
      platform: "instagram",
      collectedAt,
      account: {
        id: returnedAccountId,
        label: config.accountLabel ?? null,
        username: account.username ?? null,
        followers: count(account.followers_count, "user.followers_count"),
      },
      unreadMessages: unavailable(
        "The official Instagram Conversations API reference does not document unread_count or an account-wide unread counter. Message lists and inbound webhook counts do not represent unread inbox state.",
        "Instagram Conversations API",
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
          : "Pagination reached maxPages; recent posts may exist on unvisited pages.",
        items: [...posts.values()].sort((a, b) =>
          b.publishedAt.localeCompare(a.publishedAt),
        ),
      },
    },
  };
}
