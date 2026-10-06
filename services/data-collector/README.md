# DataCollector

DataCollector runs user-authored Node ESM scripts on fixed intervals, by manual
request, or from authenticated JSON inputs. Its Web UI edits task definitions and
scripts, shows execution status, and browses current and retained results including
camera images. English and German UI copy follows the browser language.

## Tasks and results

The [schema](src/schema.ts) defines settings, tasks and the two MCP tools:
`data_collector_read` and `data_collector_update`. Read tasks for service limits,
save a complete task with its `expectedRevision` (zero on creation), and use
enable, disable, run or delete. Mutation calls carry the same inner and routed
`operationId`; inspect the original operation after a lost reply. Receipts are
retained for seven days; do not retry an old operation after that window.

Each task exports `default async ({ config, secrets, state, input, signal })` and
returns `{ data, state?, events?: [{ name, payload }] }`. `data` is any JSON value.
`state` is private durable task state; it advances only after successful publication.
Use assigned secret names, never literal credentials in task configuration or output.
The source [setup guide](../../docs/examples/data-collector/README.md) explains
provider credentials, token renewal and private task-local authentication caches.
Scripts can report `authentication_required`, `interaction_required`,
`configuration_invalid` or `provider_unavailable` through a thrown error's `code`.
Only these fixed codes cross the worker boundary; arbitrary error messages are hidden.
Each event is published on `data-collector.event`; consumers deduplicate on
`runId` and `eventIndex`. The complete event, including collector metadata, must
fit Hive's 4,096-byte UTF-8 limit. All events are checked before writing a result;
an oversized event fails the run and leaves the task editable. No user notification is sent unless a separate subscriber
is configured to handle the event.

One Hive root contains one result object per task. Each successful run writes a
new revision; a failed or cancelled run leaves the last successful result current.
Task definitions, scheduling, state and pending publication are stored in the
instance's `data-collector.sqlite`; include this database in instance backups.
Results live in Hive. Deleting a task clears its stored definition's script, configuration and state,
removes its private `.auth` directory and hides it from the API and scheduler. Its internal record keeps the result link
and retention policy; recreating the same ID reuses that result object. Active,
disabled and deleted tasks use the same retention cleanup.

Execution status comes from the latest run, which is retained until a newer run
replaces it. Older execution receipts expire after seven days. Pending results
and events resume publication after restart without running the script again;
successful publication commits task state and run completion together.
Invalid content or a conflicting result target ends publication with an error;
temporary failures and uncertain replies remain pending for reconciliation.

Task retention is capped by the configured service limits, initially 100 MiB,
1,000 revisions and seven days. Any exceeded limit makes older revisions eligible.
Cleanup runs after publication and every minute, including for disabled tasks.
Hive always preserves the current revision and externally referenced revisions;
the UI reports references that prevent cleanup. Results must fit Hive's 1 MiB
JSON document limit. Camera examples use bounded JPEG data URLs.

## Execution and installation

Scripts run in separate Node processes with individual working directories and
npm dependencies pinned to exact versions. Dependency installation uses
`--ignore-scripts`; packages requiring installation hooks need host preparation.
Configure `npmCliPath` if npm is not installed beside Node. Task timeouts, V8 heap
limits, a service concurrency cap and owned process trees contain task failures.
Windows uses the existing Job Object launcher; Linux uses process groups.
This is process isolation for trusted user scripts, not a hostile-code sandbox:
scripts retain the service OS user's file and network permissions, and V8 heap
limits do not bound every native allocation. Run the service as a dedicated OS
user. Service credentials are not passed to scripts.

Set `intervalSeconds: 0` for manual/input tasks. Optional `settings.ingress`
starts a standalone Node HTTP listener on its configured `host` and `port`.
It accepts JSON at `POST /tasks/<id>/run`; optional `ingress.routes` maps extra
exact paths to task IDs. Built-in task paths keep their original meaning.
Devices connect directly to this listener. It runs in the DataCollector process
and needs no Hive webserver route, reverse proxy or forwarding service.

An enabled task accepts HTTP input only when it sets `inputSecretName` or
`allowUnauthenticatedInput: true`. The former requires
`Authorization: Bearer <token>` using the named value in `settings.secrets`;
the latter accepts requests without credentials. These options are mutually
exclusive. With neither option, HTTP input is disabled. The
[appliance templates](../../docs/examples/data-collector/home/README.md#appliance-setup)
explicitly use unauthenticated input on the device network.

HTTP 202 means the input was saved in the existing local SQLite run queue,
including while Hive is disconnected. Queued input survives a restart;
execution and publication resume when Hive reconnects. There is at most one
queued or active run per task, so further observations receive 409 until it
finishes. A stable `Idempotency-Key` prevents duplicate acceptance within the
receipt window; senders may omit it. Input is limited to 256 KiB.
Disabling cancels an active process; changes and
deletion require the task to finish first. A run interrupted by an abrupt service
exit is not replayed and needs the termination check below before it can run again.

If process termination returns `outcome_unknown`, the task stays blocked for
run, save, enable and delete, including after a service restart. Other tasks can
continue. Stop and verify all of that task's workers on the host first. Then read
its `lastRunId` and current revision and call `data_collector_update` with
`action: "disable"`, `id`, `expectedRevision`, a new `operationId`, and
`confirmedStoppedRunId: "<lastRunId>"`. This is the operator's explicit confirmation
of actual termination; it does not kill or inspect processes. It leaves the task
disabled, preserves its state/results/authentication cache, and records that the
original execution outcome remains unknown. A normal disable or a repeated old
confirmation cannot unlock another run. Inspect external effects before retrying.

Build the selected `data-collector` and `data-collector-ui` deployment components.
The matching Hive release must support `objects.pruneRevisions`. Protect
`/settings/secrets` with the instance's `secretPaths` configuration. Begin with all
tasks disabled and verify credentials, package identity, Hive registration and
the actual provider interaction before enabling schedules.

The [examples](../../docs/examples/data-collector/README.md) include heating, laundry,
Tesla delivery, cameras, social metrics and KDP earnings with their provider requirements.
Materialize a template with:

```sh
node tools/operations/data-collector-task.mjs docs/examples/data-collector/cameras/rtsp.task.json
```

The helper expands `@file:` into source text before submitting it as `task.script`.
The runtime does not interpret file references. A minimal script is:

```js
export default async ({ config, signal }) => {
  const response = await fetch(config.url, { signal });
  if (!response.ok) throw new Error("Collection failed");
  return { data: await response.json() };
};
```
