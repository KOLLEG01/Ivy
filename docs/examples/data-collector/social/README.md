# Social account metrics

Materialize each `.task.json` with the [task template helper](../../../../tools/operations/data-collector-task.mjs) before creating a task; `@file:` is a source directive. Use one task per account, with separate IDs and secret references. Templates start disabled and require no npm dependencies. Provision secrets in private service settings, list their names in `secretNames`, and verify a manual run before enabling a schedule. Fixtures do not verify live credentials or provider approval.

`lookbackDays` defaults to 30. Views, likes and comments are current lifetime counts for posts published in that window, not engagement earned during those days. Counts have `status`, `value` and `source`; unavailable counts have `value: null` and a reason. `posts.complete: false` identifies capped or unavailable coverage. `maxPages` defaults to 100, with a maximum of 1000 within the task timeout and provider quota.

| Collector                     | Account/content                        | Minimum grant for these metrics                                                                               |
| ----------------------------- | -------------------------------------- | ------------------------------------------------------------------------------------------------------------- |
| Instagram Login               | Business/Creator; feed posts and Reels | `instagram_business_basic`; add `instagram_business_manage_insights` for views                                |
| Instagram with Facebook Login | Professional account linked to a Page  | `instagram_basic`, `instagram_manage_insights`, `pages_read_engagement`; `pages_show_list` for Page discovery |
| TikTok Display API            | Authorized user's public videos        | `user.info.basic`, `user.info.stats`, `video.list`                                                            |
| YouTube Data API v3           | Owner channel's video uploads          | `https://www.googleapis.com/auth/youtube.readonly`                                                            |

Instagram insights can lag by 48 hours. `views` counts organic Instagram views, excluding promoted/ad interactions. Stories and album-child insights are outside this collector. Unsupported metrics/permissions produce unavailable views; authentication and rate-limit failures fail the run. `collectInsights: false` skips insights. Media pages are traversed because ordering is unsupported. [Meta insights](https://developers.facebook.com/documentation/instagram-platform/reference/instagram-media/insights)

TikTok excludes private videos and post types not exposed by Display API. Descending creation order permits the date cutoff. [User fields/scopes](https://developers.tiktok.com/docs/en/tiktok-api-v2-get-user-info), [video pagination](https://developers.tiktok.com/docs/en/tiktok-api-v2-video-list)

YouTube subscriber counts are rounded down to three significant figures; omitted/hidden counts are unavailable. Owner grants can include private/unlisted uploads. Community posts are outside video resources. Pagination is fully traversed because publication order can differ from upload order. [Channel statistics](https://developers.google.com/youtube/v3/docs/channels), [video dates/statistics](https://developers.google.com/youtube/v3/docs/videos)

## Initial authorization

Prefer an existing provider access token. These APIs use OAuth account grants, rather than a generic personal access token issued from an account password. A YouTube API key cannot authorize the owner-only `mine=true` request used here. Usernames/passwords cannot replace these grants.

Collection, refresh and token exchange use HTTP without browser dependencies. First-time or renewed consent can require the owner to use the provider's authorization page/dashboard. These scripts cannot perform that consent without user interaction or bypass login challenges/two-factor authentication. No request publishes posts, sends messages or marks messages read.

### Instagram token and account ID

1. Use a Business or Creator account; consumer accounts are unsupported. Create a Business app in the [Meta App Dashboard](https://developers.facebook.com/apps), add Instagram, and add the account you own/manage. Standard Access covers added accounts you own/manage; other owners require Advanced Access and App Review. [Setup/access levels](https://developers.facebook.com/documentation/instagram-platform/instagram-api-with-instagram-login/business-login)
2. Under **Instagram → API setup with Instagram business login**, select **Generate token** for the account and complete the owner login. Dashboard tokens last 60 days. Record the actual issuance time. [First-call guide](https://developers.facebook.com/documentation/instagram-platform/instagram-api-with-instagram-login/get-started)
3. Grant `instagram_business_basic` plus `instagram_business_manage_insights` for views. A basic-only token can collect follower/media fields while insights remain unavailable.
4. Call `GET https://graph.instagram.com/v26.0/me?fields=user_id,username` with `Authorization: Bearer <token>`. Copy `user_id` into `config.accountId` as a string. The separate `id` field is app-scoped and is not the professional ID used for `/media`.
5. Store the token under `instagram_account_a_access_token`. The template's `authMode: "token"` uses it directly. For automatic renewal, retain that secret reference and set:

```json
{
  "authMode": "long_lived",
  "accessTokenSecret": "instagram_account_a_access_token",
  "accessTokenIssuedAt": "replace-with-actual-issuance-ISO8601"
}
```

`long_lived` initializes a 60-day lifetime from the supplied issuance time. It refreshes during the final seven days, respecting Meta's minimum age of 24 hours. Renewed responses supply their actual expiry; the replacement is saved before data collection. An expired token requires a new authorization. Schedule runs frequently enough to reach the renewal window. [Refresh endpoint](https://developers.facebook.com/documentation/instagram-platform/reference/refresh_access_token)

An existing Business Login flow can instead supply a one-hour token. Set `authMode: "short_lived"` and `clientSecretSecret` to a separately provisioned Instagram App Secret, also listed in `secretNames`. The collector exchanges the token once and then maintains the cached long-lived result. Find the App Secret under **Instagram → API setup with Instagram login → Business login settings**. The initial authorization-code exchange belongs to that login flow. [Token exchange](https://developers.facebook.com/documentation/instagram-platform/reference/access_token)

For an existing Facebook Login setup, set `login: "facebook"`, `authMode: "token"` and the authorized Facebook token secret. Link the professional account to a Page. Query `GET /me/accounts?fields=id,name,instagram_business_account` with `pages_show_list`; use `instagram_business_account.id` as `accountId`. Page roles assigned through Business Manager may also require `ads_management` and `ads_read` for insights. Maintain the Facebook grant separately; Instagram Login refresh does not apply to it. [Facebook setup](https://developers.facebook.com/docs/instagram-platform/instagram-api-with-facebook-login/get-started), [insight requirements](https://developers.facebook.com/documentation/instagram-platform/reference/instagram-media/insights)

### TikTok client, grant and open ID

1. Register an app in [TikTok for Developers](https://developers.tiktok.com/), add Login Kit/Display API, and request the three scopes above. Configure sandbox test users; production requires the applicable app/product/scope approval. Record **Client key** and **Client secret** from **Manage apps**. [Registration](https://developers.tiktok.com/docs/en/getting-started-create-an-app), [app review](https://developers.tiktok.com/docs/en/app-review-guidelines/)
2. Register a callback. Web apps require an absolute static HTTPS redirect URI. Launch `https://www.tiktok.com/v2/auth/authorize/` with `client_key`, `response_type=code`, the comma-separated scopes, registered `redirect_uri`, and a fresh anti-forgery `state`. The owner grants access; validate the returned state before accepting the code. [Login Kit](https://developers.tiktok.com/docs/en/login-kit-web/)
3. Exchange the code server-side: `POST https://open.tiktokapis.com/v2/oauth/token/`, content type `application/x-www-form-urlencoded`, with `client_key`, `client_secret`, `grant_type=authorization_code`, `code`, and the identical `redirect_uri`. Desktop/mobile flows also require their PKCE verifier. Retain `access_token`, `refresh_token`, expiry values, and `open_id`. Set `config.openId` to that ID, not a username. [Code exchange](https://developers.tiktok.com/docs/en/oauth-user-access-token-management)
4. The template takes the 24-hour access token as `tiktok_account_a_access_token`. To renew automatically, remove the access-token field and replace it with the following configuration. Replace `secretNames` with these three secret names:

```json
{
  "authMode": "refresh",
  "clientKeySecret": "tiktok_oauth_client_key",
  "clientSecretSecret": "tiktok_oauth_client_secret",
  "refreshTokenSecret": "tiktok_account_a_refresh_token"
}
```

Refresh mode renews on first use, then reuses the cached access token until its final five minutes. Returned refresh tokens may rotate and are saved before profile/video requests, even when collection fails. The initial refresh grant lasts 365 days; subsequent responses provide the current refresh expiry. Expired/revoked grants require owner consent again. Avoid a second token manager independently rotating the same grant. [Refresh lifecycle](https://developers.tiktok.com/docs/en/oauth-user-access-token-management)

In `authMode: "refresh"`, any remaining `accessTokenSecret` field is ignored. Its secret need not be assigned to the task; replacing or removing that unused reference/value does not invalidate the rotated refresh cache.

### YouTube client, offline grant and channel ID

1. Choose/create a project in [Google Cloud Console](https://console.cloud.google.com/), enable **YouTube Data API v3**, configure the OAuth consent screen/audience, and add the owner as a test user while testing. Create a **Web application** OAuth client with your callback. Provision its client ID/secret as `youtube_oauth_client_id` and `youtube_oauth_client_secret`. Public user-data apps can require verification. [Prerequisites](https://developers.google.com/youtube/v3/guides/auth/server-side-web-apps#prerequisites)
2. Authorize the owner, selecting the intended channel/Brand Account where applicable. At `https://accounts.google.com/o/oauth2/v2/auth`, send `client_id`, exact `redirect_uri`, `response_type=code`, the read-only scope above, `access_type=offline`, and a fresh validated `state`. Use `prompt=consent` when obtaining a new offline grant. Exchange the code at `https://oauth2.googleapis.com/token` with `grant_type=authorization_code`, `code`, `client_id`, `client_secret`, and the same callback. Store its refresh token as `youtube_channel_a_refresh_token`. [Offline authorization](https://developers.google.com/identity/protocols/oauth2/web-server#offline)
3. Using the access token, call `GET https://www.googleapis.com/youtube/v3/channels?part=snippet&mine=true`. Copy the intended result's `id` to `config.channelId`. A handle/email/channel URL is not this ID. [Owner lookup](https://developers.google.com/youtube/v3/docs/channels/list)
4. The template refreshes each run using its three secrets. For an externally managed access token, remove the refresh/client fields and set `accessTokenSecret` with its single `secretNames` entry. External consent screens in Testing normally issue seven-day refresh tokens for these scopes; use the appropriate production status/verification for unattended collection. Google can revoke/expire grants. [Expiry rules](https://developers.google.com/identity/protocols/oauth2#expiration)

## Unread-message boundaries

`unreadMessages` remains explicitly unavailable:

- **Instagram:** the current official Conversation reference lists metadata but no supported unread counter. The generic SDK's `UnifiedThread.unread_count` does not establish support for Instagram Login or Instagram-filtered Page conversations. Inactive Requests conversations can be omitted after 30 days. Message history/inbound totals do not equal unread inbox state. Messaging permissions are therefore unnecessary here. [Conversation fields](https://developers.facebook.com/docs/graph-api/reference/conversation), [Conversations API limits](https://www.postman.com/meta/messenger-platform-api/folder/22794852-255610cd-47f5-4f4d-b3fa-71aec360be9a)
- **TikTok:** Display API has no inbox endpoint. Separate Business Messaging access requires an eligible Business Account and approved business integration, which a Display grant does not supply. A read-only unread-counter contract has not been verified for that product; this collector does not substitute conversation totals or private login APIs. [Display API](https://developers.tiktok.com/docs/en/display-api-overview), [Business Messaging](https://business-api.tiktok.com/portal/bm-api/education-hub)
- **YouTube:** Data API v3 has no direct-message inbox/unread endpoint. Comments are a separate metric. [API resources](https://developers.google.com/youtube/v3/docs)

## Private cache and failures

Renewal uses `.auth/instagram.json` and `.auth/tiktok.json` under each task's private working directory. Direct-token modes create no cache. Directories use mode `0700`; replacement files use `0600`, a flushed temporary file and atomic rename. On Windows, restrict the directory with the service account's ACL because Unix mode bits do not provide equivalent protection.

Caches contain tokens/expiries; keep them out of source control and Hive. A hash binds each cache to its account/auth configuration and provisioned credentials. Replacements invalidate reuse; unrelated settings such as `lookbackDays` preserve rotation. Keep rotated TikTok caches across restarts: the initial refresh token may no longer work. For lost/corrupt caches or changed auth settings, restore a secure backup or provision a fresh grant. Deleting a task clears its private auth directory; recreation needs a usable grant.

Credentials never appear in returned `data`, `state` or `events`. Provider error text is not retained. Relevant failures use the runtime's fixed `authentication_required`, `configuration_invalid`, or `provider_unavailable` codes. Cancellation reaches each HTTP request. Missing/revoked grants require newly authorized tokens.
