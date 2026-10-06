# Dashboards

Dashboards is an optional service and Web UI for agent-authored HTML backed by
Hive objects. Definitions are versioned `dashboards/dashboard` objects. The
service owns CRUD and checks workspace membership and update/delete revisions.
Mutations preserve the caller's operation identity across reconnects.

The [service registry](../services/dashboards/src/schema.ts) defines the compact
MCP tools: `dashboard_list`, `dashboard_read`, `dashboard_save`, `dashboard_delete`
and `dashboard_render`. The Web UI additionally uses the data snapshot tool.

## Authoring

Each definition contains `title`, `html`, `sources` and `refreshSeconds`. Sources
map a name to `{objectId}` or `{contractKey, limit}` (up to 100 current objects).
Object sources yield their JSON/text content; contract sources yield arrays.
Archived objects and host configuration are not dashboard sources. A failed source
fails the snapshot rather than silently presenting an incomplete result.

Use inline CSS and JavaScript, and embed assets as data URLs. Register the drawing
function before document load:

```html
<h1 id="heading"></h1>
<script>
  dashboard.onUpdate(async ({ data, output }) => {
    document.getElementById("heading").textContent = data.weather.summary;
    // output.mode: interactive | image
    // output.width / output.height: viewport pixels
    // output.colorMode: color | grayscale | monochrome
  });
</script>
```

Return a promise for asynchronous drawing. The image renderer waits for that
promise, fonts, embedded images and paint. A render error or timeout fails the
request. Dashboards should fit the requested viewport; images are not full-page
captures. Drawing callbacks run sequentially; if updates arrive during drawing,
the next callback receives the latest snapshot. Browser viewers refresh without
overlapping data requests, using the saved interval (default five seconds; zero
disables polling). Each image request
reads current data once.

HTML runs in a sandboxed frame without same-origin access or Hive credentials.
Hive serves published `*.sandbox.html` assets with `sandbox allow-scripts` and
permits embedding from its own origin. Normal UI documents keep `script-src 'self'`.
External fetches and assets are blocked by CSP; the image renderer also blocks
network requests. Only trusted authors should write dashboard code. Host secrets
must never be placed in dashboard definitions or ordinary data sources.

## Images and URLs

`dashboard_render` accepts `id`, `width`, `height` and `colorMode`. It returns
`mimeType: image/png`, `encoding: base64`, `data` and the selected dimensions/mode.
MCP carries the PNG once as an image block, with metadata in the structured result.
The default is 800 × 600 in color. Grayscale and monochrome are applied to the
final opaque PNG pixels; monochrome contains only black and white, without
dithering. Rendering uses one browser with a fresh context per request and admits
one concurrent image; callers retry `resource_busy`.

The authenticated Hive REST JSON-RPC `tools.call` transport can call
`dashboards.render` and receives base64 PNG plus `metadata`. The public image
endpoint is `GET /api/v1/dashboards/image.png?node=…&id=…&token=…` (also `HEAD`).
It accepts `width`, `height` and `colorMode` and returns raw PNG bytes without
requiring a Hive session. Each dashboard's optional `imageToken` grants access
only to that image; null disables access. Tokens must be random 32–128 character
URL-safe strings and are encoded in the URL. Rotation immediately revokes the
previous token. Management operations remain authenticated.

Saved `metadata` is a JSON object. The response adds `refreshSeconds` from the
dashboard refresh setting; metadata cannot override it. PNG responses include
`X-Dashboard-Refresh-Seconds` and URI-encoded JSON `X-Dashboard-Metadata`. The
same headers accompany conditional `304` responses when PNG pixels are unchanged.
Metadata occupies at most 8000 URI-encoded bytes. Tokens never enter response
metadata or data supplied by another dashboard definition used as a source.
Grayscale and monochrome PNGs use eight-bit grayscale without alpha or interlacing,
so native E Ink clients can display them directly. Hive forwards only the
read-only, token-checked `dashboards.image` handler on this public HTTP route.

Every saved dashboard exposes a stable `/dashboards/#/view?node=…&id=…` URL
that shows only the dashboard and requires the normal Hive login. The management
UI lists, edits, previews, deletes and opens dashboards and downloads PNGs.
The editor also sets metadata, enables or rotates image tokens, and shows the
token-bearing PNG URL. Contract 1.1.0 adds metadata and image access; existing
1.0.0 revisions remain readable, and explicit saves write 1.1.0.

## Installation

Build and publish `dashboards` and `dashboards-ui` using the normal component
deployment paths. Service settings are `rootObjectId` (optional, defaults to the
Hive root) and `chromiumExecutable` (optional absolute Chromium executable).
Without the latter, install the Chromium version expected by the pinned
`playwright-core` package for the service account. Chromium must be able to launch
with its sandbox enabled. Service readiness checks the renderer as well as Hive
registration. No browser is downloaded during service startup.

For Windows services, keep Chromium in an explicit Ivy runtime directory and set
`chromiumExecutable` to that executable. Verify it from the managed service, since
a browser available in an interactive test cache may not be visible to the
scheduled service.

Implementation: [service](../services/dashboards/src/main.ts),
[viewer](../ui/dashboards-ui/src/Viewer.vue),
[focused checks](../tests/dashboards.test.ts).
