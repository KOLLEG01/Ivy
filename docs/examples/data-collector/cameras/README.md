# Camera collectors

The disabled task templates use `@file:` only for the task materializer; the
runtime requires the resulting script source. Use the
[task materializer](../../../../tools/operations/data-collector-task.mjs), replace
the neutral camera/account ids, and provision only the selected service secrets.
Use one task per camera to choose its interval and keep the result below the Hive
JSON limit. Both scripts require Node 24. Blink password authorization uses the
template's exact `tough-cookie` and `parse5` dependencies; RTSP uses Node built-ins.
Keep tasks disabled until a manual run verifies the account and camera.

## Blink

`blink.mjs` implements the unofficial OAuth v2 HTTP flow and camera endpoints in
current [blinkpy authentication](https://github.com/fronzbot/blinkpy/blob/dev/blinkpy/auth.py)
and [API source](https://github.com/fronzbot/blinkpy/blob/dev/blinkpy/api.py).
It prefers an existing OAuth access token, refreshes when a matching refresh
token/device identity is available, and falls back to email/password when selected.
Password login uses PKCE and a cookie session without a browser. Legacy
`token-auth` credentials are unsupported. This is an undocumented provider API;
fixtures do not establish availability or access to a real account.

### Start with a Blink account

1. Use the email and password of the Blink account that owns the cameras. Obtain
   them from the owner's existing account/password manager; verify the account's
   current email and access to its verification channel in Blink account settings.
   No developer app or external token manager is needed for this path.
2. Provision private service secrets `BLINK_EMAIL` and `BLINK_PASSWORD`. In a copy
   of `blink.task.json`, replace `secretNames` with
   `["BLINK_EMAIL", "BLINK_PASSWORD"]`; unselected secret names are not passed to
   the script. Remove `accountId` and `region`, set `discoveryOnly: true` and
   `cameras: []`. Keep the same task id throughout authorization.
3. Materialize the edited template and create the disabled task. Run it manually.
   If Blink requests MFA, the run reports `interaction_required` and the provider
   may send a verification code through the account's configured channel. The
   private cookie/PKCE challenge survives that failed run; later runs resume it
   without repeating password signin.
4. Put the received code into private service secret `BLINK_MFA_CODE` and add that
   name to the same task's `secretNames`. Run that task again. A supplied code is
   attempted once, including an uncertain network outcome; a rejected code needs
   a newly received code. Do not put codes in run input, task config or comments.
5. A successful discovery result provides `data.accountId`, `data.region`, and
   `data.cameraInventory` entries with `id`, `networkId`, `type` and `name`. Copy
   the selected entry into `config.cameras`, set `discoveryOnly: false`, and
   optionally copy accountId/region into config. Use exactly the returned type:
   `camera`, `mini`, or `doorbell`. Remove the MFA secret from task selection once
   authorization succeeds. Verify a collection run before scheduling it.

A pending challenge expires locally after 15 minutes. If it expires, stop the
schedule and remove the task's private `.auth/blink.json` on the collector host to
start authorization again. Deleting the task also removes its authentication
directory. The adapter does not resend codes or generate TOTP from a seed; the
verified protocol uses a received verification code. CAPTCHA, external security
redirects or other unsupported challenges report `interaction_required` and need
owner/provider action. A password alone cannot bypass those checks.
An uncertain password-signin response also requires private-cache recovery;
it may already have triggered a code. Rejected passwords are not submitted again
until the supplied credentials change or the private cache is removed.

### Use existing OAuth credentials

If the owner already has an authorized Blink client session, use its OAuth
`access_token` (`token` in a blinkpy session export) as `BLINK_ACCESS_TOKEN`. The
template selects this secret by default. For automatic refresh, also provision
`BLINK_REFRESH_TOKEN` from `refresh_token` and `BLINK_HARDWARE_ID` from the **same
session's** `hardware_id` UUID, and select all three in `secretNames`. A refresh
token without its matching device identity is rejected. Do not guess an identity
or treat an old legacy token as an OAuth token. The script can also start from
refresh-token/hardware-id secrets without an access token.

Existing session metadata `account_id` and `region_id` can supply config, or omit
both fields and use discovery as above. An access token alone has no automatic
renewal; replace that secret when it expires. Select email/password as additional
fallback secrets only if that fallback is wanted.

| Config field             | Default service secret | Use                                             |
| ------------------------ | ---------------------- | ----------------------------------------------- |
| `accessTokenSecretName`  | `BLINK_ACCESS_TOKEN`   | Existing OAuth access token                     |
| `refreshTokenSecretName` | `BLINK_REFRESH_TOKEN`  | Existing session's refresh token                |
| `hardwareIdSecretName`   | `BLINK_HARDWARE_ID`    | UUID matching a supplied refresh token          |
| `emailSecretName`        | `BLINK_EMAIL`          | Account email for password fallback             |
| `passwordSecretName`     | `BLINK_PASSWORD`       | Account password for password fallback          |
| `mfaCodeSecretName`      | `BLINK_MFA_CODE`       | Received verification code during authorization |

The field values are secret **names**; credentials belong only in private service
settings. Password login creates its own stable hardware UUID. Rotated tokens are
atomically saved to task-local `.auth/blink.json` before account/camera reads, so
a later collection failure does not lose them. Files use mode 0600 and the
directory uses mode 0700; restrict the task directory with host ACLs on Windows.
Tokens, cookies and codes never enter task state, result data or events. Changing
supplied access/refresh/password/device credentials resets the cache; changing an
MFA code preserves its challenge. A revoked refresh token is remembered until
credentials change, so schedules do not repeatedly submit it. Transient errors
stop the run without starting password signin.

`authentication_required` means credentials were rejected or are missing;
`interaction_required` means owner verification is required;
`configuration_invalid` means configuration/private-cache repair is required;
`provider_unavailable` covers transport, rate limits and unsupported responses.
Vendor changes, offline cameras, account restrictions and subscription limits can
prevent collection. Do not copy a session export into tracked files or task state.

With `freshSnapshots: true`, each run requests a new thumbnail, waits for the
command and checks that the homescreen thumbnail changes before downloading it.
This wakes battery cameras and can consume battery or conflict with recording;
the example interval is five minutes. `freshSnapshots: false` downloads the last
available thumbnail and reports `fresh: false`. `capturedAt` uses the provider's
numeric thumbnail timestamp when supplied; otherwise it records retrieval time.

Set `snapshotIntervalSeconds` to separate the polling interval from camera wakes.
The default `0` requests a fresh snapshot every run. For example, poll every 30
seconds with `snapshotIntervalSeconds: 300` to wake each camera every five minutes
or when a new recording/motion event is found. Failed snapshot triggers remain
pending for the next poll. Between wakes, existing thumbnails are downloaded
without a capture command; `fresh: false` and the provider timestamp show their age.

For storage on a Sync Module, set `localStorage: true`. Active modules are
discovered from the homescreen. The collector requests their clip manifests
through Blink's backend without a storage subscription. The first successful
read establishes a baseline; subsequent reads emit `recording` events for new
clips. They trigger a snapshot only when `freshSnapshots` is enabled. This is polling after a clip
has been saved, so recording duration and the poll interval add delay.
The manifest supplies normalized camera names and no motion-source field:
ambiguous names are rejected, and these events do not assert PIR motion.
Only clips returned in the manifest are covered. The collector retains 2,000
clip identities per module and accepts up to 64 new events per run; failures
preserve the previous cursor. [Local-storage API](https://github.com/fronzbot/blinkpy/blob/dev/blinkpy/api.py)

For battery cameras, use `localStorage: true`, `localClipImages: true` and
`freshSnapshots: false`. This mode rejects configurations that enable camera
captures. It compares the saved thumbnail, the cached clip frame and the newest
local clip for each camera. The newest clip is uploaded from the Sync Module
through Blink's backend and downloaded once; no recording or
Live View command is sent to the camera. FFmpeg decodes the saved file on the
collector host and keeps its last frame. That frame replaces the available image
only when its timestamp is newer, including clips that started before the image
was taken. Without a subscription, automatic
motion-thumbnail updates are not available; the existing thumbnail is a fallback.
[Blink thumbnail support](https://support.blinkforhome.com/en_US/using-your-camera/auto-update-thumbnail)

Install FFmpeg on the collector host and set `ffmpegPath` if it is outside PATH.
`clipFrameWidth` defaults to 960, `clipFrameQuality` to 7 and
`clipFrameTimeoutMs` to 10000. `maximumClipBytes` defaults to 16 MiB, with a 32 MiB
ceiling. The final JPEG remains in task-local `.frames`, one cache file per
camera, so idle polls and temporarily unavailable manifests reuse it. Failed
downloads retry on a later poll while retaining the existing image. The image's
`source` is `blink-local-clip` or `blink-thumbnail`; `fresh` remains false because
no capture was requested. Clip `capturedAt` approximates the last frame time from
the recording timestamp plus FFmpeg's output time; `recordedAt` gives the clip's
original timestamp. A thumbnail with no known capture time reports null in this
mode. Clip completion, the poll interval and hub transfer determine the delay.

Cloud motion is inferred only from non-deleted video entries whose `source` is
`pir`. Manual clips and unsupported sources do not generate `motion` events.
The first successful poll establishes a baseline; subsequent polls deduplicate
media ids in state. Polling includes a five-minute overlap, caps recovery lookback
at one day and retains 1000 ids. Delayed clips beyond that lookback and unavailable
cloud recording can be missed. Local recordings use the separate manifest path
above. This is polling, not a real-time sensor. `data.motion.available`
and `reason` expose an unavailable media API while images can still be collected.
`maximumMotionPages` defaults to 10; a full final page preserves the prior cursor
and reports the limit instead of silently skipping unseen pages.
The result budget allows 64 new motion events per run. Larger batches also
preserve the cursor and require a shorter interval or recovery lookback.

## RTSP

Enable the camera or NVR's RTSP streaming service in its vendor configuration and
create an account with stream-viewing permission. Obtain the exact stream URI
from the vendor's model documentation or NVR channel settings. If supported,
[ONVIF](https://www.onvif.org/profiles/profile-s/) discovery and its media
[`GetStreamUri`](https://www.onvif.org/specs/stream/ONVIF-Streaming-Spec.pdf)
operation can provide it; ONVIF support and login are device
dependent. There is no universal path, port or channel number to guess. Check
the chosen URI from the collector host before using it in a task.

Install [FFmpeg](https://ffmpeg.org/ffmpeg.html) on that host with RTSP,
the camera's video decoder and MJPEG encoding enabled. `ffmpegPath` selects the
binary; it is not installed through task dependencies. Store the complete
`rtsp://user:password@camera.example/stream` URL in `RTSP_ENTRANCE_URL`; percent
encode reserved characters in credentials. Use `rtsps` only when the camera and
installed FFmpeg support it. The host needs network access to the camera.

FFmpeg returns one JPEG via stdout, without a shell or temporary image files.
Its stderr is discarded, and collector errors contain no stream URL. URL
credentials remain visible to administrators through the child process arguments.
Capture abort, deadline and excess output kill the child and wait for it to close.
Keep the task timeout above the combined capture deadlines; the service must also
stop child processes when a worker is terminated externally. RTSP video does not
provide motion events; this collector does not infer movement from image changes.
The TCP transport is the default; UDP can be selected where the network permits it.

## Image budget

Each JPEG defaults to a 512 KiB cap, adjustable up to 600 KiB; a run accepts at
most 650000 raw image bytes in total before base64 encoding. Reduce RTSP `width`
or increase `quality` (2 is highest, 31 lowest) if a frame exceeds the cap.
Blink thumbnails are not resized; an oversized thumbnail is reported as an error.
Partial camera failures appear in `data.cameras`. RTSP fails when all images fail;
Blink can still return available motion data when snapshots fail. For additional
cameras, split tasks before increasing time or memory limits. Retention applies
to the complete results, so image records can reach the byte limit quickly.
