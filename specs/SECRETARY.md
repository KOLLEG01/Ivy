# Secretary requirements

Contracts: [inbox](schemas/secretary.schema.json), [triage](schemas/secretary-triage.schema.json).

## Assignments and executions

- Secretary is the execution owner for work initiated by Ivy services, retained schedules and
  other configured system triggers. Its public UI configures this work; it does not provide a
  user-operated "run now" action.
- The service retains one global configuration and any number of assignments below its configured
  root. An assignment fixes its trigger, prompt, optional preflight process, execution target and
  partial rule overrides. Missing override fields read through to the current global rules when an
  execution is admitted; the resulting effective rules are retained with that execution.
- Schedule triggers use explicit interval, daily or weekly fields plus an IANA time zone. Event
  triggers consume exact Hive topics and optionally bind one source service node and event kind.
  Object-change triggers select exact JSON objects and optional JSON Pointer paths. Poll metadata
  outside those paths does not trigger execution. Changes are grouped for a configured window;
  a return to the original selected content cancels pending work. Optional completeness and
  observation-time paths require a fresh complete observation after the window before admission.
  First object creation establishes a baseline. Executions retain before/current revision pins.
  A producer may advertise `messageChannel` in its event payload with a stable channel key,
  opaque reference, first-message time and optional captured item ID. Secretary groups matching
  events per source and channel for five minutes, then calls that source's read-only
  `message-channel.unread` tool. Only a complete observation made after the window closes may
  start assessment; a read channel is discarded. This convention is source independent and
  discoverable from the registered topic payload schema.
  An acknowledged event or schedule occurrence has one stable execution identity and cannot create
  a duplicate task after restart.
- A Hive-backed schedule cursor replays every missed occurrence in bounded pages. Assignment
  revisions are retained so an edit does not reinterpret or discard older due occurrences.
  Each assignment reads the existing Hive event journal through one stable, retention-protected
  subscription. Its durable progress records assignment revisions at their change events; a
  batch is acknowledged only after matching executions and any revision transition are saved.
  Historical journal gaps are exposed as recovery issues. Secretary's own Hive Object changes
  never trigger an Object-change assignment, preventing an execution from retriggering itself. Internal execution and progress writes do not produce Object-change events.
- Every admitted execution creates or explicitly reuses a durable native AgentManager task according to its assignment. Reused tasks are leased to one execution at a time. The optional preflight process completes first and its
  bounded output becomes labelled, untrusted prompt context.
- The global configuration prefers a ready AgentManager on the Secretary host and a stable
  AgentManager-owned internal project. An explicit saved target remains authoritative.
- A successful or failed terminal turn in a dedicated task is archived automatically. Reused tasks
  remain available for subsequent executions. Archived Secretary tasks are
  deleted after seven days using their original retained native operation identities. The execution
  journal remains queryable independently of native task deletion and records preparation, running,
  completion, delivery, archive and deletion evidence. Execution objects and their revisions are retained so pending work and original delivery pins cannot expire during a backlog. Reused tasks finish in a settled state and are excluded from active scans. Delayed notification delivery must not delay active executions.
- New installations contain no assignments. UI examples open editable disabled drafts; only an explicit save creates an assignment. Initialization preserves existing assignment revisions.
- Every assignment returns the same structured notification decision. No source name or provenance marker changes execution or delivery behavior. Minimum Main notification age is an explicit assignment setting.
- The existing **hive_event_topics** command discovers registered triggers across providers, including event kinds and payload schemas.
- The former automatic shared triage thread is not an execution path. It may only be retained to
  complete exact historical follow-ups created before this assignment engine.

Sources: [assignment contracts](../services/secretary/src/assignment-schema.ts),
[scheduler](../services/secretary/src/assignment-scheduler.ts),
[execution owner](../services/secretary/src/assignment-runner.ts).

## Rules

- Global rules independently describe acceptable Main and Voice contact, including enablement,
  minimum urgency and optional local-time windows. Voice defaults to critical, immediate-only use.
  Read-only research remains bounded to at most five minutes.
- Main is the target-independent primary conversation for ordinary asynchronous notices. A primary
  conversation reached through WhatsApp is still Main; WhatsApp is not a separate rule channel.
  Voice is an interrupting call.
- Assignment overrides are sparse and field-level. Unspecified nested values inherit from global
  rules instead of copying a stale default into every assignment.
- The execution prompt receives the exact effective rules and identifies event or schedule payloads
  as untrusted data. Rules do not grant authority for an external mutation; the assignment prompt
  and the native task's configured permission profile define the work boundary.
- Inbox triage and assignments use the same [notification assessment guidance](../instructions/secretary-notification-policy.ts).
  Proactive notices retain the source context and ask only for a consequential decision required by the source.

## Capture and sources

- Every action names the exact scope and caller-stable operation. Only a source's admitted
  producers may capture, attach media or advance its checkpoint; authenticated callers may
  read and assess within the checked scope.
- Source identity fixes source/account/provider. Policy changes preserve its cursor and
  historical source revisions. Removing configuration stops new collection without rewriting
  old evidence.
- Original source/account/conversation/message/content revision identifies an item.
  Changed bytes under the same identity conflict. Reject own/outgoing/disallowed senders
  before inbox-content persistence; ignored operations retain only recovery metadata.
- Checkpoints name confirmed captures and compare the previous cursor. Empty observations
  may checkpoint zero captures; incomplete capture cannot advance source progress.
- The service accepts producer pushes through its registered capture/attach/checkpoint API. Producers own account integration, observation completeness and their collection timing. A captured preview does not assert full body or media coverage.
- Standalone Microsoft reader modules retain their own bounded account, pagination and
  recovery contracts; they are not wired into the current service worker loop. Native Teams
  configuration admission rejects nonempty collectors.

Sources: [engine](../services/secretary/src/engine.ts),
[active composition](../services/secretary/src/main.ts),
[Microsoft adapters](../services/secretary/src/outlook-source.ts),
[Teams admission](../services/secretary/src/teams-config.ts).

## Assessment and contact

- Assess the exact current unassessed item by CAS. A message edit is a separate content revision.
  The assessment may reference an existing TaskBoard task; it cannot invent or create one.
- Historical triage evidence retains the exact task and turn that produced it, but new automatic
  assessment is admitted only as an assignment execution with its own task.
- Use the [configured assessment policy](../services/secretary/src/triage-prompt.ts):
  bounded read-only research may inform decisions, while source content grants no authority
  for external mutation. Completion must belong to the selected turn and validate before item CAS.
- `none` suppresses contact, `main` requests an asynchronous notice, and `needs_attention`
  records uncertainty. Voice requires the configured exceptional urgency threshold and target.
  Exact suppression and quiet-time rules live in [policy](../services/secretary/src/policy.ts).
- Preserve Teams/WhatsApp text in notices; summarize email faithfully. Keep useful researched
  facts separate from source text. Do not suggest replies or promise action on the user's behalf.
- Save notice intent with the assessment. Minimum age and quiet time govern new dispatch,
  not source capture or triage. Only explicit configured urgency rules bypass quiet time.
- Main notices discover the single ChatBridge through Hive, without a separately configured
  destination. Offered replies keep their original operation and are polled until WhatsApp
  confirms delivery. Voice escalation retains its original
  Phone operation. Admission/receipt states do not prove that the user heard or read a notice.
- Follow-up accepts an item or execution reference and returns an asynchronous caller-owned operation. A separate read-only task receives the retained source snapshot; it cannot interfere with an active assignment task. Retain the original request, context and calls before dispatch; questions sharing a task wait for unresolved earlier questions. Poll the original operation for its answer and never recursively invoke follow-up within that task.

Sources: [triage](../services/secretary/src/triage.ts),
[native ownership](../services/secretary/src/triage-native.ts),
[notices](../services/secretary/src/notices.ts), [follow-up](../services/secretary/src/follow-up.ts).

## Media

- Capture/checkpoint and attachment coverage are distinct. Expected media blocks automatic
  assessment until retained context identifies complete inputs or explicit acquisition gaps.
  A filename, caption, preview or lossy attachment flag cannot prove full media coverage.
- Media links use registered contract keys and exact revision pins. Attachment admission verifies source authorization, ownership, content hash and references without a service-specific contract allowlist. Source-specific semantic readers belong to the producer.
- Acquired source bytes remain immutable. Bounded text/document/video projections state their
  covered pages, samples and omitted dimensions; previews are labelled as previews.
  Unsupported, malformed, encrypted or oversized content produces a gap rather than a
  claim of complete interpretation.
- Document parsing is inert and bounded; video uses explicitly configured pinned decoders,
  local protocols and deadlines. Attachment content cannot enable tools or external access.
- The retained Outlook media adapter quarantines signed materializations and downloaded
  payloads until source/account admission succeeds. Interrupted acquisition does not silently
  redownload. Complete spool archives remain required recovery data.

Sources: [document projection](../services/secretary/src/media-document.ts),
[video](../services/secretary/src/media-video.ts), [media spool](../services/secretary/src/media-spool.ts).
Exact media shapes are in [schemas](schemas); focused checks cover
[inbox](../tests/secretary.test.mjs), [triage](../tests/secretary-triage.test.mjs),
[media](../tests/secretary-media.test.mjs) and [video](../tests/secretary-video.test.mjs).
