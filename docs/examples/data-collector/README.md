# DataCollector source setup

All source scripts run through Node without browser automation. Authorization is
provider-specific: an OAuth access token is not a permanent personal access token
(PAT). Some providers require a one-time interactive OAuth consent during setup.
The collectors do not replace that consent with an account password.

| Source                       | Authentication                                                          | Setup instructions                                    |
| ---------------------------- | ----------------------------------------------------------------------- | ----------------------------------------------------- |
| Fröling                      | Existing bearer token, or Connect username/password                     | [Heating](home/README.md#froling-setup)               |
| Washer / dryer               | Direct, unauthenticated JSON push to DataCollector's configured port    | [Appliances](home/README.md#appliance-setup)          |
| Tesla active orders          | Registered Fleet API application and authorized refresh token           | [Tesla Fleet API](home/README.md#tesla-fleet-setup)   |
| Tesla delivery HTML          | Explicitly provisioned account session cookie; experimental             | [Tesla account HTML](home/README.md#tesla-html-setup) |
| Blink                        | OAuth token/refresh token, or email/password with provider verification | [Cameras](cameras/README.md)                          |
| RTSP                         | Camera stream URL with camera credentials                               | [Cameras](cameras/README.md#rtsp)                     |
| Instagram / TikTok / YouTube | Account-authorized API tokens; supported refresh flows                  | [Social accounts](social/README.md)                   |
| KDP                          | Amazon credentials, or an explicitly provisioned session; experimental  | [KDP](kdp/README.md)                                  |

## Store credentials

1. Obtain the credentials and IDs using the provider guide. Keep one task per
   account or camera, with distinct task IDs and secret names.
2. In the owning host's authoritative Ivy HostConfig, find the DataCollector
   instance. Add credential values to that instance's `settings.secrets` and add
   `/settings/secrets` to its `secretPaths`. Preserve its other settings and secret
   paths. Use the normal host configuration workflow; generated local instance
   files are overwritten by reconciliation. See [configuration](../../../specs/DEPLOYMENT.md#configuration).
3. In the task, put only **names** in fields such as `passwordSecret`, and include
   every used name in `secretNames`. Remove names belonging to auth modes you are
   not using: the runtime requires every assigned secret to exist. Authenticated
   ingress uses `inputSecretName`; its token need not be exposed to the script.
   Appliance push tasks instead set `allowUnauthenticatedInput: true` and need no
   secret.

For example, the following is an **instance configuration fragment**, not a task:

```json
{
  "secretPaths": ["/settings/secrets"],
  "settings": {
    "secrets": {
      "froeling_username": "<Connect account email>",
      "froeling_password": "<Connect account password>"
    }
  }
}
```

Store real values through a private configuration channel or local secret file;
do not put them in task JSON, script source, chat, Git, or collected results.
Task-local OAuth/session caches live in `tasks/<task-id>/.auth` under the service
work directory. They survive service restarts and are written before subsequent
collection, so rotated refresh tokens survive failed data requests. Protect and
back up these files with the service credentials. Files use mode 0600 and their
directory mode 0700 where supported; Windows protection depends on the service
account's directory ACL. Deleting a task removes its `.auth` cache; it does not
remove named secrets from the instance configuration or revoke provider grants.

## Create and verify a task

1. Copy a matching disabled `*.task.json` template and replace its neutral IDs and
   config values. Keep `enabled: false` during setup. The provider guide explains
   discovery modes where the API can supply the IDs.
2. Materialize its script from the adjacent file:

   ```sh
   node tools/operations/data-collector-task.mjs docs/examples/data-collector/home/froeling.task.json
   ```

   Submit that JSON as `task` in `data_collector_update` with `action: "save"`,
   `expectedRevision: 0` and a new operation ID. For edits, use the revision from
   `data_collector_read` with `view: "task"`. In the Web UI, put the source in the
   script editor and the remaining task fields in the settings editor. The runtime
   does not expand `@file:` references itself.

3. Use `action: "run"` or **Run now** while the task is disabled. Inspect status and
   current result with the two DataCollector MCP tools or the result viewer.
   Verify account identity, fields, units, timestamps and provider limitations.
4. Use `action: "enable"` only after a successful manual run. Set interval and
   retention per task; the service's maximum retention still applies. Appliance
   push tasks use interval zero and must be enabled to accept ingress requests.

`authentication_required` means credentials/token must be renewed;
`interaction_required` means the provider requires verification (for example a
Blink code or Amazon CAPTCHA). Follow the source guide and run again after the
required action. Credentials and codes never belong in the run's JSON input.
`configuration_invalid` identifies missing/invalid settings; `provider_unavailable`
identifies an unavailable or unsupported provider response. A failed run keeps the
last successful revision and its original collection time, not a new zero result.

Instagram/TikTok/YouTube examples report unavailable inbox counts explicitly where
the selected API has no supported unread-message metric. Their post counts are
lifetime counters for posts within the configured publication-date window.
Fixture tests verify the implementation; they do not establish access to a real
account. KDP CAPTCHA challenges and Tesla client-rendered order pages can still
prevent browserless collection after configuration.
