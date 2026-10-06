const failure = (code, message) => Object.assign(new Error(message), { code });

export default async function collectYouTube({ config, secrets, signal }) {
  const days = config.lookbackDays ?? 30;
  const maxPages = config.maxPages ?? 100;
  if (!Number.isInteger(days) || days < 1 || days > 365)
    throw failure("configuration_invalid", "lookbackDays must be 1..365.");
  if (!Number.isInteger(maxPages) || maxPages < 1 || maxPages > 1000)
    throw failure("configuration_invalid", "maxPages must be 1..1000.");
  const collectedAt = new Date().toISOString();
  const end = Date.parse(collectedAt);
  const start = end - days * 86400000;
  const unavailable = (reason, source) => ({
    status: "unavailable",
    value: null,
    reason,
    source,
  });
  function count(value, source) {
    if (typeof value !== "string" || !/^\d+$/.test(value))
      return unavailable("The API did not return this count.", source);
    const number = Number(value);
    return Number.isSafeInteger(number)
      ? { status: "available", value: number, source }
      : {
          status: "available",
          value,
          representation: "decimal_string",
          source,
        };
  }
  function secret(name) {
    const value = secrets[name];
    if (typeof value !== "string" || !value.trim())
      throw failure(
        "authentication_required",
        "A configured OAuth secret is missing.",
      );
    return value;
  }
  async function fetchJson(url, options) {
    signal?.throwIfAborted();
    let response;
    try {
      response = await fetch(url, { ...options, signal, redirect: "error" });
    } catch (error) {
      signal?.throwIfAborted();
      throw failure(
        "provider_unavailable",
        "YouTube OAuth/API request failed before a response was received.",
      );
    }
    let body;
    try {
      body = await response.json();
    } catch {
      throw failure(
        "provider_unavailable",
        `YouTube OAuth/API returned invalid JSON (HTTP ${response.status}).`,
      );
    }
    if (!response.ok || body.error) {
      const code =
        body.error === "invalid_grant" ||
        body.error === "invalid_client" ||
        response.status === 401 ||
        body.error?.errors?.some(
          (item) => item.reason === "insufficientPermissions",
        )
          ? "authentication_required"
          : body.error?.errors?.some(
                (item) => item.reason === "accessNotConfigured",
              )
            ? "configuration_invalid"
            : "provider_unavailable";
      throw failure(
        code,
        `YouTube OAuth/API error (HTTP ${response.status}). Check the OAuth grant, API enablement, and quota.`,
      );
    }
    return body;
  }
  let token;
  if (config.refreshTokenSecret) {
    const body = new URLSearchParams({
      client_id: secret(config.clientIdSecret),
      client_secret: secret(config.clientSecretSecret),
      refresh_token: secret(config.refreshTokenSecret),
      grant_type: "refresh_token",
    });
    const result = await fetchJson("https://oauth2.googleapis.com/token", {
      method: "POST",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body,
    });
    if (typeof result.access_token !== "string" || !result.access_token)
      throw new Error("Google OAuth did not return an access token.");
    token = result.access_token;
  } else token = secret(config.accessTokenSecret);
  async function get(path, params) {
    const url = new URL(`https://www.googleapis.com/youtube/v3/${path}`);
    for (const [key, value] of Object.entries(params))
      url.searchParams.set(key, String(value));
    return fetchJson(url, { headers: { Authorization: `Bearer ${token}` } });
  }
  const channels = await get("channels", {
    part: "snippet,statistics,contentDetails",
    mine: true,
    maxResults: 50,
  });
  if (!Array.isArray(channels.items) || channels.nextPageToken)
    throw new Error(
      "YouTube returned an invalid or ambiguous owner channel list.",
    );
  const account = config.channelId
    ? channels.items.find((item) => item.id === config.channelId)
    : channels.items.length === 1
      ? channels.items[0]
      : undefined;
  if (!account || typeof account.id !== "string" || !account.id)
    throw new Error(
      "The OAuth grant must identify the intended channel. Set channelId if multiple owner channels are returned.",
    );
  const uploads = account.contentDetails?.relatedPlaylists?.uploads;
  if (typeof uploads !== "string" || !uploads)
    throw new Error(
      "YouTube did not return an uploads playlist for the owner channel.",
    );
  const posts = new Map();
  const unavailableIds = new Set();
  const seenVideoIds = new Set();
  const seenPages = new Set();
  let pageToken;
  let complete = false;
  for (let page = 0; page < maxPages; page++) {
    const params = {
      part: "contentDetails",
      playlistId: uploads,
      maxResults: 50,
    };
    if (pageToken) params.pageToken = pageToken;
    const result = await get("playlistItems", params);
    if (!Array.isArray(result.items))
      throw new Error("YouTube returned an invalid uploads playlist.");
    const ids = [];
    for (const item of result.items) {
      const id = item.contentDetails?.videoId;
      if (typeof id !== "string" || !id)
        throw new Error("YouTube returned an upload without a video ID.");
      if (!seenVideoIds.has(id)) {
        seenVideoIds.add(id);
        ids.push(id);
      }
    }
    if (ids.length) {
      const videos = await get("videos", {
        part: "snippet,statistics,status",
        id: ids.join(","),
        maxResults: 50,
      });
      if (!Array.isArray(videos.items))
        throw new Error("YouTube returned an invalid video list.");
      const returned = new Set();
      for (const video of videos.items) {
        if (!ids.includes(video.id))
          throw new Error("YouTube returned an unexpected video ID.");
        returned.add(video.id);
        const published = Date.parse(video.snippet?.publishedAt);
        if (!Number.isFinite(published))
          throw new Error(
            "YouTube returned a video without a valid publication time.",
          );
        if (video.snippet.channelId !== account.id) {
          unavailableIds.add(video.id);
          continue;
        }
        if (published < start || published > end) continue;
        posts.set(video.id, {
          id: video.id,
          publishedAt: new Date(published).toISOString(),
          title:
            typeof video.snippet.title === "string"
              ? video.snippet.title
              : null,
          url: `https://www.youtube.com/watch?v=${encodeURIComponent(video.id)}`,
          visibility: video.status?.privacyStatus ?? null,
          views: count(
            video.statistics?.viewCount,
            "video.statistics.viewCount",
          ),
          likes: count(
            video.statistics?.likeCount,
            "video.statistics.likeCount",
          ),
          comments: count(
            video.statistics?.commentCount,
            "video.statistics.commentCount",
          ),
        });
      }
      for (const id of ids) if (!returned.has(id)) unavailableIds.add(id);
    }
    // Upload order is not publication order: an old private upload can become public today.
    if (!result.nextPageToken) {
      complete = true;
      break;
    }
    if (
      typeof result.nextPageToken !== "string" ||
      seenPages.has(result.nextPageToken)
    )
      throw new Error(
        "YouTube returned an invalid or repeated pagination token.",
      );
    seenPages.add(result.nextPageToken);
    pageToken = result.nextPageToken;
  }
  const subscribers =
    account.statistics?.hiddenSubscriberCount === true
      ? unavailable(
          "The API reports that the channel's subscriber count is hidden.",
          "channel.statistics.subscriberCount",
        )
      : count(
          account.statistics?.subscriberCount,
          "channel.statistics.subscriberCount",
        );
  if (subscribers.status === "available")
    subscribers.precision =
      "rounded down to three significant figures by YouTube";
  return {
    data: {
      platform: "youtube",
      collectedAt,
      account: {
        id: account.id,
        label: config.accountLabel ?? null,
        title: account.snippet?.title ?? null,
        subscribers,
      },
      unreadMessages: unavailable(
        "YouTube Data API v3 has no direct-message inbox or unread-message count endpoint.",
        "YouTube Data API v3",
      ),
      window: {
        days,
        start: new Date(start).toISOString(),
        end: collectedAt,
        counts: "lifetime totals for posts published in the window",
      },
      posts: {
        complete: complete && unavailableIds.size === 0,
        reason: !complete
          ? "Pagination reached maxPages; recent publications may exist on unvisited upload pages."
          : unavailableIds.size
            ? "Some uploads were not returned or no longer belong to this channel; their publication time and counts are unavailable."
            : null,
        unavailableIds: [...unavailableIds],
        items: [...posts.values()].sort((a, b) =>
          b.publishedAt.localeCompare(a.publishedAt),
        ),
      },
    },
  };
}
