# Deployment and configuration

Contracts: [component manifest](schemas/component.schema.json), [host schema](schemas/host.schema.json).
Implementation: [host runtime](../packages/host-runtime/src), [build tooling](../tools/build).
Local commands and operating procedure: [host operations](#local-host-operations).

## Releases

- Build one selected component from one fixed source snapshot. Keep the checkout stable
  during capture; later edits cannot alter a prepared candidate. Preparation and tests
  are separate from activation.
- A release is immutable and contains only the selected component and declared runtime
  dependencies. Requirements describe supported runtimes/platforms; configuration, secrets,
  databases and sessions remain outside it.
- Package versions use strictly increasing numeric SemVer per component. Reusing a version
  with changed bytes or manifest is rejected; the highest accepted version is desired.
- Upload authorization is short-lived, single-use and bound to exact component, version,
  build, hash and size. Publish a catalog revision only after validating the complete
  archive and making it available.
- One Hive Object holds the versioned package catalog. Apps use the same package path;
  Hive installs them in its app catalog. HostExecutor activates only configured executable
  components and verifies downloaded bytes/manifest without rebuilding on the target.
- A failed exact release is not retried without a new version or explicit retry.
  Hive releases are packages, not container images; any container lifecycle owner must
  remain outside the replaced Hive process.
- Package reconciliation includes bootstrap-owned components through an independent OS
  handoff and stopped-owner maintenance, with readiness and rollback. Returning offline
  hosts converge from the catalog without a central push.
- HostExecutor collects completed build preparations, unreferenced source snapshots and
  obsolete candidate directories after three days. It retains the two newest versions
  per component, live selections, their previous successful build and nearest older
  version, and candidates referenced by unresolved operations or preparation evidence.
  Bootstrap history keeps current, rollback and unresolved maintenance plans plus the
  grace period; older plans no longer pin all historical releases. Candidate deletion
  is journaled and retried after interruption. Eligible preparations and snapshots are
  atomically renamed with a `.retiring-` prefix before deletion; interrupted removal
  resumes from those names even if their metadata has gone. Failed and unknown
  preparations remain for diagnosis; unknown ownership pauses source snapshot collection only.
  Completed bootstrap detail files expire after 30 days, keeping details for the
  latest 20 maintenance operations, current rollback evidence and unresolved operations.
  Small request and outcome records remain unchanged for replay and restore; their
  number grows with operation history. Only detail files named in those records are deleted.
  Host status includes the last collection outcome, last complete success, deletion
  counts, free space and grouped reasons for retained entries. A partial
  result explicitly reports blocked areas. Free space is checked every minute;
  below 2 GiB or 10% free, safe collection runs early, at most once per hour.
  Normal collection remains daily; pressure never relaxes reference or grace rules.
  Automatic collection does not scan payloads for byte statistics. Explicit preparation
  inventory still measures sizes; manual compaction reports best-effort measured bytes
  (zero when unavailable), and measurement failures never block verified cleanup.
- Publish a HostExecutor release whose manifest remains readable by the oldest deployed
  executor before publishing component manifests with new fields. An older executor reads
  catalog identities and verifies that HostExecutor release first, then hands it to OS
  bootstrap maintenance. The updated executor validates and installs the other releases.

## Configuration

- Hive is authoritative for each complete HostConfig; writes use revision conflict checks.
  Hive retains its own local bootstrap settings so startup does not depend on its API.
- HostExecutor is the sole host reconciler. ServiceManager consumes durable changes for
  that host's config and package catalog and forwards local hints. A periodic authoritative
  read must recover lost hints and event gaps.
- Validate the complete new config against the fixed installation before atomically replacing
  the local last-known-good file. Invalid/unreachable config leaves the previous file active.
- Verify Hive's content hash against the supplied configuration, then resolve account defaults
  on the target host. The effective local configuration has its own hash because defaults can
  add paths that were omitted from the central document.
- Ordinary instance changes use journaled activation; bootstrap-owned processes use their
  separate handoff. Services receive their own generated config and do not reconcile Hive
  HostConfigs independently.
- Process owners reload and validate the selected instance configuration before every launch.
  Windows breakaway is an explicit process-instance setting, never inferred from a component
  name. Component reset manifests may declare one marker inside instance data; the host writes
  its completed reset identity and the component applies domain cleanup idempotently before
  readiness.
- Browser projections always protect credentials and explicitly declared `secretPaths`.
  Markers bind instance ID and JSON Pointer; arrays are protected as complete values.
  Browser Object reads must not bypass this projection or expose secrets through queries.

See [configuration reconciliation](../packages/host-runtime/src/configuration-updater.ts),
[change hints](../services/service-manager/src/main.ts) and
[browser projection](../services/hive/src/configurations.ts).

### Service defaults

The host [configuration resolver](../packages/host-runtime/src/layout.ts) applies
[portable service settings](../packages/host-runtime/src/default-settings.ts) to
omitted fields. Explicit values, including empty lists, `false` and `null`, win.
Unknown extension settings pass through unchanged. Defaults do not add or enable
instances. Use a separate account or explicit directories when sharing a host.

| Component                     | New-configuration defaults                                                                                                                                                                        | Required local setup                                                                                              |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| HostExecutor / ServiceManager | This host's config path; reconciliation every 60 seconds; Windows ServiceManager breakaway                                                                                                        | Host identity, Hive URL, credentials and executable paths                                                         |
| Hive                          | Loopback port 39081; daily backup, seven copies under `<ivyRoot>/backups/hive`                                                                                                                    | Fresh credentials, package publishers and HTTPS edge/proxy addresses                                              |
| AgentManager                  | `<ivyRoot>/codex/<instanceId>` and its `skills` folder; Windows external proxy, Linux owned stdio; patching enabled; 512 MiB journal, 100,000 operations, 64 pending inputs, 16 MiB notifications | Supported native executable/version/hash, sign-in, Windows shell when applicable; optional project/home overrides |
| TaskBoard                     | Scheduler every two seconds, pages of 50; Sol 6.1 / xhigh / standard, automatic host selection, Main chat contact, no worktrees or parallel work                                                  | Principal; a ready compatible AgentManager and ChatBridge for Main contact                                        |
| ChatBridge                    | One-second polling, English fallback                                                                                                                                                              | Channel/account, verified native project and plan, WhatsApp configuration; optional language                      |
| Secretary                     | Two-second polling, two records per tick; five-minute research, normal Main urgency, questions-only WhatsApp, no voice escalation                                                                 | Identity/scope and explicit sources; native execution configuration                                               |
| DataCollector                 | 512 MiB task memory ceiling, empty secret inventory; existing retention/concurrency defaults                                                                                                      | Task scripts and any assigned secrets or ingress                                                                  |
| Dashboards                    | No preselected parent object                                                                                                                                                                      | Verified Chromium executable                                                                                      |
| PhoneBridge                   | SIP port 5070 on loopback, UDP; EVS/G722/PCMA/PCMU, 20 ms packets; 60-second ringing, one-second polling; empty recipient/incoming policies                                                       | Windows audio, native/Voice setup, SIP account, protected credentials and permitted callers/recipients            |

Account-, device- and network-specific values remain mandatory installation inputs.
Configure non-loopback listeners and trusted proxies explicitly when required by the
host topology. Verify optional services before enabling them. TaskBoard's initial
preferences apply only until a configuration is saved in Hive; supported models
are checked against the selected host's live catalog.

## First public installation

The minimal supported installation is a released public distribution with Hive,
HostExecutor and ServiceManager. Hive includes the Console. AgentManager,
TaskBoard and the public automation example may be configured while disabled, but
they are not prerequisites; account, device and private-integration components
are outside this path.

Prerequisites are a supported host/runtime from the selected
[component manifests](schemas/component.schema.json), absolute writable state,
artifact and staging roots, and an HTTPS edge for any non-loopback installation.
The edge forwards one exact base path to Hive's loopback listener. Set the
canonical external `publicBaseUrl` to that HTTPS URL and admit only the exact edge
addresses as trusted proxies. DNS, certificates, ports, executable paths and
storage roots are local installation inputs, never distribution defaults.

Use this order:

1. Create a local `HostConfigInput` conforming to the
   [host contract](schemas/host.schema.json). Generate a fresh host ID, principal
   ID and high-entropy credential locally; no required credential is supplied by
   the public artifacts. Start every instance disabled. Protect the complete Hive
   credentials array with its explicit `secretPaths` entry. The portable resource
   defaults exercised by the installed acceptance are in
   [fresh-config.mjs](../tests/acceptance/scenarios/fresh-config.mjs); the script
   generates fresh identities and contains no deployment configuration.
2. Run the local CLI `status` against that exact file to validate and resolve its
   layout. Prepare the HostExecutor and Hive candidates from one fixed public
   source/distribution snapshot, recording their returned candidate IDs. Install
   the HostExecutor with `install-bootstrap`, then deploy and enable the exact Hive
   candidate. [Host operations](#local-host-operations) defines the commands and terminal phases.
3. Configure the initial principal as a package publisher. For every later public
   candidate, use the MCP `hive_authorize_package_upload` tool with the exact
   prepared manifest/archive identity, upload bytes only to its returned one-time
   path, and confirm the release in `packages.catalog`. The
   [operation schema](schemas/hive-operations.schema.json) is authoritative for
   the request and result fields. Host package reconciliation consumes that same
   catalog; it does not rebuild releases on the target.
4. Deploy and enable ServiceManager through the durable executor, restart it
   through the same CLI path, and wait for the original deployment to reach
   `succeeded`. Verify authenticated `system.status`, the ServiceManager node, and
   load the Console at the configured base URL after signing in with the locally
   generated credential. Only then add and enable optional services with their own
   verified local inputs.

The public installed acceptance in
[fresh-install.mjs](../tests/acceptance/scenarios/fresh-install.mjs) follows this
bootstrap order, restarts ServiceManager, and verifies both the live service node
and the bundled Console. It consumes prepared public candidates and does not
substitute a development build for an installed artifact.

## Activation and recovery

- Accept the original request durably before stopping a process. Preserve its operation ID,
  exact candidate and previous build. Activation is serialized on a host and survives the
  initiating CLI/service exiting.
- Check compatibility against actual retained formats/contracts before switching. Drain,
  stop, launch and readiness checks are bounded and use the platform's process-tree ownership.
  Unrelated components remain supervised.
- Reconcile unfinished steps against observed state after restart. Ambiguity is
  `needs_attention`; rollback is a distinct outcome. Process survival is not readiness.
- Roll back binaries only when they can read current data. Never restore an old database
  automatically after a failed health check.
- Backups use a consistent Hive snapshot or verified stopped host owners. Restore into fresh
  isolated roots; preserve unknown external outcomes, clear release/live selections and
  leave host instances disabled until explicit deployment and enablement.
- A periodic Hive SQLite snapshot is paired with a same-name `.ui` directory containing
  its private UI release files. Restore the pair together; the database alone cannot serve
  file-backed UI releases. Backup copies omit upload staging directories. Incomplete
  backup copies and unpaired UI directories are collected after a day; backup retention
  removes both halves of every expired pair.
- Shared native homes remain external data as specified in [agent environments](AGENT-INSTRUCTIONS.md).
  Destructive runtime reset is a separate explicit, coordinated maintenance operation:
  verify its exact deletion/preservation plan and stopped owners before execution.

## Local host operations

Run the installed `ivy` CLI, or `node dist/packages/cli/src/main.js` from the checked
distribution, on the owning host. Use `--config ABSOLUTE_PATH --json` for every command
(or the installation's explicit `IVY_HOST_CONFIG`). First read `status` and confirm the
host ID, instance IDs, installed builds and unfinished operations; do not change hosts
because a call failed. The CLI works without Hive. Exact request and result shapes are in
the [host schema](schemas/host.schema.json).

| Command                                                                                                                          | Purpose                                                                                          |
| -------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `status [--deployment ID]`                                                                                                       | Read host state or the original deployment record, including readiness and preparation progress. |
| `prepare --component ID --source PATH`; `preparations`; `compact-preparations`                                                   | Capture an immutable candidate; inspect or compact retained preparations.                        |
| `deploy --instance ID (--candidate ID \| --source PATH) --operation-id ID`                                                       | Durably request activation; prefer deploying the exact inspected candidate.                      |
| `restart`, `enable`, `disable` with `--instance ID --operation-id ID`; `rollback --instance ID --to BUILD --operation-id ID`     | Change an instance through the executor.                                                         |
| `install-bootstrap --candidate ID`; `inspect-bootstrap`; `update-bootstrap --request PATH`; `bootstrap-status --operation-id ID` | Install or maintain bootstrap-owned processes separately from ordinary deployment.               |
| `backup --destination PATH`; `restore --backup PATH --backup-hash HASH`                                                          | Snapshot or restore an offline host.                                                             |

Preserve the exact request, operation ID, candidate/build ID and returned deployment ID.
After acceptance, read `status --deployment ID` until `data.record.phase` is terminal:
`succeeded`, `failed`, `needs_attention` or `rolled_back`. `--wait-ms N` on lifecycle
commands waits at most 600000 ms; timeout or connection loss does not cancel an accepted
request. Retry only with its original operation ID and arguments. Exit 0 can mean
acceptance while work is pending; exit 1 means failure or rollback, 2 invalid arguments,
and 3 `needs_attention`. Diagnose that state and verify the intended build and affected
user flow before reporting success.

For `update-bootstrap`, preserve the exact plan identity and verify affected owners are
stopped. Keep the current Linux aggregate resource budget unless the task changes it.
Do not alter unrelated services or discard recovery evidence to pass a check. For backup
and restore, verify offline owners and use a fresh isolated destination; inspect backup
SQLite only from a verified copy. Restore preserves private data but clears candidates,
releases, targets and live observations, and disables every instance. Resolve any
`needs_attention` history, then prepare, deploy and enable explicitly. Keep snapshots
and credentials outside release artifacts.

Recovery implementations and focused checks: [backup/restore](../packages/host-runtime/src/restore.ts),
[runtime reset](../packages/host-runtime/src/runtime-reset.ts),
[deployment tests](../tests/executor.test.ts), [package tests](../tests/package-deployment.test.ts).
