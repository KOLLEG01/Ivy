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
