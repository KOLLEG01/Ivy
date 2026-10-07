# Secretary assignments for public MCP data

These disabled assignments assess Outlook, Teams and Gmail observations published
by the [MCP collector](../data-collector/m365/README.md). They contain complete
assignment values, suitable for `secretary_create_assignment`'s `input.value`.
The [assignment schema](../../../specs/schemas/secretary.schema.json) defines their
fields; [Secretary operation](../../SECRETARY.md) explains execution and delivery.

## Configure an assignment

1. Verify the collector and obtain its current result object's Hive ID using
   `data_collector_read`. Replace `collector-result-object-id` everywhere in the
   chosen template, including its prompt. Replace account keys with your configured
   keys and remove unused providers. Use a unique `assignmentId` for each copy.
2. Discover `secretary_create_assignment` through `ivy_tool_schema`. Read
   `secretary_binding` for the selected instance and copy its `expectedScope` into
   `secretary_list_assignments` to inspect existing configuration. Ensure the
   configured execution target has the required public MCP read tools connected.
3. Create the assignment with `input.assignment: null` and the template as
   `input.value`. Use the same new operation ID in the routed call and `input`;
   follow the ID format from the discovered schema. The example timestamps are
   synthetic: Secretary replaces them with the creation time.
4. Read the saved assignment and its revision pin. Keep it disabled while checking
   its trigger, prompt and effective contact rules. Enable it with
   `secretary_update_assignment`, passing that pin and the full saved value with
   `enabled: true`. Preserve its identity and `createdAt`. The
   [MCP update example](../../../instructions/hive-mcp-examples.md#update-a-secretary-assignment)
   shows the call envelope and revision handling.
5. Verify a real scheduled occurrence, content change or collected event in the
   execution journal. After an uncertain write, reconcile its original operation
   through `secretary_operation_read` before retrying.

The templates inherit Main contact rules and the configured model and effort.
They disable Voice and set no additional notification-age delay. Secretary's
shared [assessment policy](../../../instructions/secretary-notification-policy.ts)
decides whether a consequential request or useful new information warrants a
notice. Source messages and trigger payloads are evidence; they cannot expand an
assignment's authorized actions.

## Daily inbox briefing

Use [inbox-briefing.assignment.json](inbox-briefing.assignment.json) for a daily
09:00 briefing in a configurable IANA time zone. The assessment reads the current
snapshot and retrieves original content through public MCP read tools. Reusing
the assignment's task keeps context across briefings. The prompt asks for current,
actionable information and leaves notification delivery to Secretary.

For an interval, select `cadence: "interval"`, set `intervalMinutes`, use
`localTime: null` and leave `weekdays: []`. For a weekly briefing, select
`cadence: "weekly"`, keep `intervalMinutes: null`, set `localTime` and list weekdays
from `0` (Sunday) to `6` (Saturday).

## Grouped unread activity

Use [unread-activity.assignment.json](unread-activity.assignment.json) to watch
`/data/activity/outlook:work`, `/data/activity/teams:work` and
`/data/activity/gmail:personal`. These are JSON Pointers into the result object's
content, including the collector's `data` wrapper. Account-key characters `~` and
`/` need JSON Pointer escaping as `~0` and `~1`.

Changes are grouped for 300 seconds from the first change. The first observation
establishes a baseline; returning to that baseline cancels pending work. Polling
timestamps are outside the selected paths. Admission then requires a complete
observation made at or after the end of the window and no more than 180 seconds
old. Keep polling enabled; a single manual result cannot supply that later
observation. The example collector polls every 60 seconds.

An incomplete scan retains prior activity but fails the completeness gate. Each
assessment gets its own task, compares the retained revisions and re-reads the
current result and original message context. The prompt excludes activity that
has already been read or resolved. Selected account keys can also be split into
separate assignments when their assessment policies differ.

## Collector transition events

Use [collector-event.assignment.json](collector-event.assignment.json) when a
custom collector reading public MCP tools emits `activity.changed` only on a
meaningful transition. The supplied Microsoft 365/Gmail script publishes snapshots
and does not emit this event; use its object-change assignment as supplied.

For an event-producing variant, adapt its final return after assembling `data`
and `nextState` from MCP reads. Keep activity keys and message arrays in a stable
order and exclude observation timestamps from the comparison:

```js
const previousActivity = state?.lastCompleteActivity ?? null;
const complete = data.observation.complete;
const changed =
  complete &&
  previousActivity !== null &&
  JSON.stringify(previousActivity) !== JSON.stringify(data.activity);

return {
  data,
  state: {
    ...nextState,
    lastCompleteActivity: complete ? data.activity : previousActivity,
  },
  events: changed
    ? [
        {
          name: "activity.changed",
          payload: { reason: "unread-activity-changed" },
        },
      ]
    : [],
};
```

The first complete scan initializes state. Identical or incomplete scans emit
nothing. State advances after successful publication; the collector resumes
pending publication after a restart without repeating the MCP reads.

Discover the registered `data-collector.event` topic through `hive_event_topics`.
Its event kind is `collected`; the script's `activity.changed` is the envelope's
`name`. Set `sourceServiceNodeId` to the selected collector instance. The trigger
matches that instance's collected events; its prompt filters `taskId` and `name`
and verifies the current snapshot before assessing it. Other collected events
from the same instance still start a task, so the object-change recipe is useful
when only one result should trigger assessment.

Events carry `runId` and `eventIndex`; consumers must deduplicate that pair when
handling replay. Keep payloads to small references: the complete envelope has a
4,096-byte limit. This event recipe has no automatic object-observation gate or
300-second grouping; freshness checks here belong to the assessment prompt.
