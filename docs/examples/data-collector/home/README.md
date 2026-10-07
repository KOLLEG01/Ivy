# Home collection tasks

Materialize `*.task.json` with the DataCollector task helper to embed the adjacent
script. Templates are disabled and contain no account configuration. Assign only
the listed secrets before enabling a task. Successful runs return JSON observations,
persistent transition state and optional events; no adapter sends notifications.

| Template                   | Source                          | Required configuration                                                      |
| -------------------------- | ------------------------------- | --------------------------------------------------------------------------- |
| `froeling.task.json`       | Fröling Connect HTTP            | Bearer token or account secrets, user/facility IDs, installed component IDs |
| `washer.task.json`         | Appliance JSON push             | DataCollector's standalone HTTP port; no token required                     |
| `dryer.task.json`          | Appliance JSON push             | DataCollector's standalone HTTP port; no token required                     |
| `tesla-fleet.task.json`    | Tesla Fleet API active orders   | Registered application, authorized refresh token and expected account email |
| `tesla-delivery.task.json` | Experimental Tesla account HTML | Order-page URL and assigned account session-cookie secret                   |

Fröling uses the web login and component-read protocol, which has no public API
contract. Verify the account, facility, component IDs and returned fields against
the installation before enabling. It preserves named component readings and emits
`heating.threshold.crossed` once below each configured threshold, rearming at or
above the threshold. Authorization headers are neither stored nor returned.

The laundry tasks have `intervalSeconds: 0` and `allowUnauthenticatedInput: true`.
Send the original device JSON body directly to `POST /tasks/washer/run` or
`POST /tasks/dryer/run` on DataCollector's own HTTP port without an Authorization
header. The optional ingress must be configured on a device-reachable interface.
Washer bodies
provide `cycle_st`; dryer bodies provide `cycle_timer` and `step_timer`.
`tte_showed` supplies remaining seconds when available. `timestamp`, when present,
is Unix seconds. The first observation
establishes a baseline. Washer state 11 and dryer remaining time transitioning from
a positive value to zero emit `laundry.finished`; duration changes emit
`laundry.status.changed`. The sentinel 65535 means unknown remaining time and cannot
produce a dryer completion. Older timestamps do not replace the latest reading.
Persisted task state carries the last appliance payload between pushes and restarts.

The Tesla HTML adapter is experimental and does not implement a verified backend
order API. Normal [Tesla account login](https://www.tesla.com/teslaaccount) requires
a browser-generated CAPTCHA token; password/TOTP credentials alone cannot establish
a Node HTTP session. Assign a pre-provisioned authenticated session-cookie secret.
The adapter performs no login or page JavaScript execution.

It extracts English or German delivery windows from server HTML and emits
`tesla.delivery.changed` when a previously observed window changes. Redirects may
change the locale but must retain the configured RN order on the same origin;
dashboard and other-order redirects fail without collecting their contents. Auth
redirects require session renewal; a client-rendered shell without a delivery
window fails explicitly and needs a verified order API. This undocumented account
surface must be checked with an authenticated response before enablement; HTML
fixtures do not establish backend API parity or live availability.
The documented [Fleet API user endpoints](https://developer.tesla.com/docs/fleet-api/endpoints/user-endpoints)
include active orders. Their documentation does not guarantee an estimated-delivery
field; inspect an authenticated response before mapping delivery data.

## Froling setup

1. Create/sign in to a [Fröling Connect account](https://connect-web.froeling.com/)
   and associate your heating installation, following the manufacturer's
   [Connect manual](https://connect.froeling.com/en/support/B1080522_froeling-connect_en.pdf).
   Use an account already authorized for that installation. Its account email and
   password become the `froeling_username` and `froeling_password` secrets. No
   separate PAT-creation flow is known for this proprietary API.
2. To discover IDs without developer tools, set `config.discoveryOnly: true`,
   remove the placeholder `userId` and `facilityId`, and run the disabled task
   manually. It reads `userId` from the login token, then returns facility IDs and
   component IDs. Copy the intended IDs into the normal template, select the
   installed components, and remove `discoveryOnly` before collecting telemetry.
   Discovery does not change threshold state.
3. Run the task and inspect returned `components.*.values` before choosing
   threshold metric names. Component IDs and available measurements depend on
   the installation; the template's boiler/buffer/circuit IDs are examples.

If an authorized integration has already issued a bearer token, store it as a
secret, select its name with `config.accessTokenSecret`, and assign that name in
`secretNames`. Preserve the exact `Authorization` header value issued by the login,
including `Bearer ` if present. This is an expiring login token, not a PAT. The source prefers it and retries once
with username/password on HTTP 401 if both account secrets are also assigned.
Token-only tasks report `authentication_required` after expiry. The proprietary
discovery paths are described by the independent
[froeling-connect implementation](https://github.com/Layf21/froeling-connect/blob/main/src/froeling/endpoints.py).

## Appliance setup

These templates accept the appliance sender's JSON protocol directly. The path is
`device -> DataCollector HTTP listener -> task -> Hive result`; no forwarding
service or manufacturer's cloud login is involved.

1. Materialize the washer and dryer templates. They explicitly allow input without
   a token; leave `inputSecretName` unset and `secretNames` empty.
2. Configure the listener in the DataCollector instance settings, for example:

   ```json
   {
     "ingress": {
       "host": "0.0.0.0",
       "port": 8091,
       "routes": {
         "/api/pushStateWM": "washer",
         "/api/pushStateD": "dryer"
       }
     }
   }
   ```

   `host` selects the local interface; `port` is independent of Hive's HTTP port.
   The optional `routes` entries accept those exact device paths in addition to
   `/tasks/washer/run` and `/tasks/dryer/run`.

3. Point the devices at `http://<collector-host>:8091/api/pushStateWM` and
   `http://<collector-host>:8091/api/pushStateD`, respectively, using
   `Content-Type: application/json`. No token, login, proxy or Hive URL is needed.
   These endpoints are intentionally unauthenticated for the device network.
4. Enable the input tasks after checking their configuration and payloads. Send
   an observation and inspect the resulting state. HTTP 202 confirms local queue
   storage even while Hive is unavailable; execution waits for reconnection.
   Another observation receives 409 while that task has queued or active work.
   Retry it later. An optional stable `Idempotency-Key` deduplicates retries.

The JSON fields and completion transitions are described above. For other input
tasks that need authentication, use `inputSecretName` instead of
`allowUnauthenticatedInput` as described in the
[service guide](../../../../services/data-collector/README.md#execution-and-installation).

## Tesla Fleet setup

`tesla-fleet.task.json` reads `/api/1/users/me` to verify `expectedEmail`, then
`/api/1/users/orders`. It returns the active-order list without assuming undocumented
delivery fields. A first observation establishes a baseline; later order changes
emit `tesla.orders.changed`. API list/key reordering does not count as a change.
It uses no browser during collection and sends no vehicle commands.

1. Obtain credentials for a registered application through
   [Tesla app onboarding](https://www.tesla.com/support/third-party-app-onboarding).
   A consumer account alone does not supply application credentials. Application
   registration belongs to the developer; the product owner separately grants read
   access through Tesla's consumer authorization flow. Do not enroll the product
   owner in Tesla for Business or an installer program. Register an HTTPS callback
   URI and the read scopes `user_data` and `vehicle_device_data`. Complete the
   application's [regional registration](https://developer.tesla.com/docs/fleet-api/endpoints/partner-endpoints),
   including hosting its public key. Regions `eu` and `na` use their respective
   Fleet API origins; this template does not implement China's separate authorization.
2. Put the application's `clientId` and `clientSecret` in a private local JSON file.
   The helper follows Tesla's [third-party OAuth flow](https://developer.tesla.com/docs/fleet-api/authentication/third-party-tokens):

   ```sh
   node tools/operations/tesla-fleet-authorize.mjs begin --client-file /private/client.json --redirect-uri https://example.com/tesla/callback --region eu --session-file /private/authorization.json
   ```

   Open its `authorizationUrl`, sign in and grant the requested read access. The
   callback URI must exactly match the application registration. Save the complete
   resulting redirect URL in a private text file; do not put its code in chat or
   command arguments. The callback page itself need not exchange or display tokens.

   ```sh
   node tools/operations/tesla-fleet-authorize.mjs finish --client-file /private/client.json --session-file /private/authorization.json --callback-file /private/callback.txt --output /private/collector-secrets.json
   ```

   The helper verifies the callback URI and state, exchanges the code at Tesla's
   Fleet authentication server, and writes the client ID and refresh token only to
   the new private output file. Output files are never overwritten. Delete temporary
   callback/session files after successful provisioning.

3. Add `tesla_client_id` and `tesla_refresh_token` to the owning collector's protected
   `settings.secrets`, using the [source setup guide](../README.md#store-credentials).
   Set the task's `region` and actual `expectedEmail`, then materialize and save it
   disabled. Run manually and verify the returned account and order list before
   enabling its 30-minute schedule.

Rotated tokens live in the task's private `.auth/tesla-fleet.json`, committed before
data requests. The cache survives failed collection and restarts. Changing bootstrap
credentials or region starts a new authorization cache. The client secret is needed
only for initial code exchange and is not assigned to the task. Expired/revoked grants
require authorization again; a rejected API access token triggers at most one renewal
per run. HTTP 403 reports `interaction_required`: check application registration and
granted access. HTML/challenge responses are errors, never order observations.

After verifying an actual response, optional
`orderSelector: {"path":"/referenceNumber","value":"RN..."}` selects an order, and
`deliveryPointer` maps its observed field using a JSON pointer. These are explicit
mappings, not assertions about Tesla's response schema. Delivery reports
`not_configured`, `order_not_found`, `field_missing` or `available`; only an available
string populates `estimatedDelivery`. A changed string emits `tesla.delivery.changed`.
If the API does not expose the delivery window, this collector cannot replace a
portal-derived delivery observation.

## Tesla HTML setup

The accepted exception is a manually provisioned account session. The collector
itself uses Node HTTP and does not open a browser.

1. Sign into your [Tesla account](https://www.tesla.com/teslaaccount), complete its
   verification, and open the order detail page containing your `RN...` number.
   Copy that exact HTTPS URL to `config.orderUrl`.
2. In your browser's Network developer tools, reload that order page and select
   its document request to `www.tesla.com`. Copy only the request's **Cookie header
   value** into the private `tesla_session_cookie` secret. Do not copy headers into
   chat or export unrelated browser cookies. The task references the secret name
   with `config.cookieSecret`.
3. Run manually. A login redirect requires renewing this secret; an HTML shell
   without delivery details means this account page cannot be read by the current
   adapter. A successful fixture test does not prove your live page is server-rendered.

There is no automatic password/CAPTCHA fallback in this adapter. A Tesla Fleet
API token for vehicle telemetry cannot be used as an order-delivery token.
