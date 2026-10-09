# TaskBoard requirements

Contracts: [schema](schemas/task-board.schema.json), [operations](schemas/task-board.operations.json).
Implementation: [runtime](../services/task-board/src/runtime).
UI requirements: [UI](UI.md).

## Work and allocation

- A Task is the durable ticket; Runs are execution attempts. Its stable task key also identifies
  per-task output directories. Workflow state, execution condition and user/agent control are separate.
  Category is organizational metadata, not a capability or execution target.
- Permanent deletion removes the Task, its attachments, all Hive revisions, and task-linked
  Run, Result, Review, History, Delivery and comment-read records. It is refused while a claim
  or publication is active, or another Task still depends on it. The deleted task key is not reused.
- Archiving a ticket also archives its linked Codex tasks, including previous Run and History contexts.
  Restoration tries the current native task first; confirmed failure creates a new native task
  while retaining the ticket's workflow state. Pending or unknown native outcomes never create
  replacements. Shared archive controls and MCP use the TaskBoard lifecycle action.
  Done cards offer Archive task; the Done column offers Archive all tasks for the complete
  filtered column, including cancelled tickets and tasks beyond the current page.
- Explicit `newThread` work creates a new Codex task and queues the existing ticket in Todo.
  Previous contexts remain linked until ticket archival. It requires resolved execution; ordinary continuation reuses the
  current native task. Ticket content, comments, attachments and previous Runs remain available.
- User-controlled tasks never start autonomously. Agent-controlled Todo work is eligible only
  when dependencies, timing, an available host and workspace conditions permit it. A missing
  execution requirement inherits the TaskBoard execution default; an explicit automatic
  requirement allocates to the least busy eligible AgentManager;
  an explicit host narrows that pool. Required capabilities (shown as the Task's dependencies)
  must all be advertised by the host that runs it. An unavailable environment or missing
  capability leaves unstarted work in Todo with an explicit condition. Records from before
  required capabilities read a single-capability requirement as automatic placement that
  requires that capability.
- Host and workspace are ordinary ticket fields until the first attempt. Afterwards the native
  context lives there, so only the explicit reassign workflow moves the Task.
- Users can return any unclaimed, non-cancelled Task to the unscheduled Backlog.
- TaskBoard owns one revision-checked `task-board/configuration` below its workflow root.
  It stores default model, reasoning effort, service tier and execution requirement separately
  from HostConfig. Settings changes require no service restart. Null task selections inherit
  these defaults; explicit standard speed clears the native service tier. Resolve defaults
  before the first allocation and retain them in the native Run plan. Continuations inherit
  that plan rather than later default changes; explicit task choices still take precedence.
- Creation and changes to execution selections validate the effective host, model, reasoning
  and speed before publishing ticket fields. Model IDs and reasoning must match the selected
  host's complete live catalog; aliases are not inferred. Explicit native selections require
  a ready host. Allocation rechecks availability because catalogs can change after saving.
  Model choices combine providers on each host; defaults spanning hosts require at least
  one compatible provider per host. Reasoning and speed follow the selected model's providers.
- Resolve capabilities and project paths from the selected host's live observations.
  Save the exact owner and canonical workspace before execution. Standard tasks share the host's
  internal root and write files under `outputs/<task-key>/`. Project and directory checkouts are
  shared, including during Review. With parallel execution disabled, reserve the shared checkout
  only while work is active or unresolved. Optional per-task worktrees retain an exclusive path
  until Done, cancellation, Backlog or archive. Release reservations after committed completion;
  on lookup, reclaim stale reservations from authoritative idle state while preserving unfinished
  admissions, publications and native outcomes. Retain the workspace files themselves.
  Agents discover peers through `task_list relatedTo`, matching the allocated canonical working
  directory and host rather than the project ID. Separate worktrees do not receive each other's
  automatic state notifications. Explicit directed messages and dependencies may cross workspaces.
  Agents coordinate overlapping changes,
  commits and deployments through directed ticket comments; unrelated changes must be preserved.
  A task may defer with `blocker: {taskId, until: "idle" | "done"}`. Cycles are rejected.
  Blocked is the board label for `waiting`, including tasks blocked before their first attempt.
  Confirmed native usage-limit failures defer agent work until the reported reset, with a one-minute
  grace period. Without a usable reset time, retry after fifteen minutes. Continue in the same context;
  repeated limits defer again. General failures, cancellation and unanswered questions require intervention.
  When the condition clears, the scheduler rechecks time, dependencies, host and workspace before
  continuing. User-controlled work and Backlog remain unscheduled. A task waiting for a host
  names why each AgentManager was skipped. Backlog rank is the ticket's Hive sibling order.
- The agent performs Git lifecycle work from the ticket's explicit workspace/worktree intent.
  TaskBoard and AgentManager do not silently clone, switch branches or alter repositories.
  Completed workspace directories are retained.

Sources: [allocation](../services/task-board/src/runtime/scheduler.ts),
[workspace resolution](../services/agent-manager/src/projects.ts),
[conditions](../services/task-board/src/runtime/condition.ts).

## Execution and conversation

- Actions use stable operation identities and expected revisions. One CAS claim authorizes
  an attempt; timeouts or expired leases cannot steal work that may already be executing.
- Retain the Run and native start intent before dispatch. Save the returned native identity
  before a turn; uncertainty never creates a replacement thread/turn.
- Before continuing a saved native context, restore it if it was archived outside TaskBoard.
  Retain and reconcile the unarchive operation before admitting the next Run; preserve the
  thread identity, owner, workspace and queued input. A failed pre-turn resume confirmed as
  archived may retry this path automatically. Active writers and unknown outcomes never
  authorize replacement contexts or duplicate turns.
- Resume and pre-turn reattachment request metadata without hydrating the complete history;
  retain every prepared request unchanged and use paginated reads for turn inspection.
- A confirmed missing thread may recover automatically only when every attempt in that context
  is conclusively unstarted: any prepared turn needs its original prevention receipt. Recheck
  restoration through a retained operation before queuing one fresh Run, preserving the owner,
  workspace, execution options and queued input. The fresh Run starts its first turn directly from
  its retained thread/start receipt; an empty thread is not resumed before that turn.
  Unknown, dispatched or previously executed turns forbid this path.
  Recovery retries retain their operation identity and wait at least 30 seconds between failures.
- An uncertain pre-turn `thread/resume` receipt may be superseded after an owner epoch change
  only when a fresh owner read confirms the same durable thread. The original receipt stays
  retained; the successor resumes that thread before the prepared turn is sent. Unavailable
  owners retain In progress with an automatic recovery condition and capped backoff.
  Claimed recovery and result publication stay In progress; requests for user input remain Blocked.
- The concise English start prompt identifies the Task and owning service through Ivy MCP.
  Agents reread the current ticket/settings/comments at every turn, work in the assigned workspace,
  post meaningful progress/questions with `authorKind=agent`, and publish non-Git files as Task attachments.
- Comments are the single conversation and handoff surface. Ordinary decisions need no separate
  approval workflow. Native requests retain their original identities in comments; UI buttons and
  MCP answer them through AgentManager. Responses are mirrored into the same timeline; secret
  answers stay at the native owner.
- User comments wake current or future agent work, except on Done: those remain notes unless
  `moveToTodo=true` explicitly reopens the ticket. Title, description and criteria cannot change
  beneath an active claim; permitted scheduling metadata changes keep revision checks.
- Update prompts contain only the task key, service reference and a request to reread the ticket;
  never copy ticket fields or comment bodies. Running turns receive `turn/steer` with their exact
  expected turn ID. Retained operation receipts acknowledge the original comment batch only.
  Unknown receipts are reconciled before another delivery; a failed steer leaves comments queued
  for the same primary's next turn. Event wakeups and periodic reconciliation share this path.
- Ordinary agent comments never wake their author. Directed messages use `authorKind=agent`,
  `sourceTaskId` and explicit `agentDelivery=queue`. Review and blocked recipients with a primary
  may answer in a coordination-only turn without implementation, reopening review or replacing
  its result. Done and Cancelled never restart from agent messages. Related-task state changes
  notify running peers through the same reference-only steering path.
- Agents publish one final comment with `purpose=handoff`, useful output links, actual verification
  and material limitations, then end the turn. TaskBoard alone moves successful execution to Review
  and sends the configured completion notification. Unanswered questions and failed attempts remain
  Blocked; queued follow-up work returns to Todo unless an explicit blocker remains. Users move Review to Done using the normal board
  transition, with no result-acceptance action or extra review state.
- Serialize publication per Task until the first Task write has durably retained the owning
  Operation marker. Root coordination additionally covers shared category and dependency graph
  changes. That marker and Task CAS then protect the unfinished publication while up to ten
  independent Tasks reconcile concurrently. Free slots continue reconciling while another Task
  awaits its owner; scans retain bounded pending pages. An ambiguous first write retains its Task gate until
  exact replay proves the marker; recovery must finish the original prepared writes. Dependency
  checks include committed marked intents, and optional worktree reservations remain unique by
  host and canonical path. Shared checkouts serialize active work unless both tasks allow parallel execution.
- Final comments retain actual verification, limitations and artifact/Git links. Internal Run outcome
  records remain available for recovery; they are not a second user-facing result or acceptance workflow.
  Unavailable evidence is not success; local commits are not represented as confirmed remote commits.
- Completion requires the exact native turn's terminal status and uses its ticket handoff comment.
  Without a handoff, read the newest output item first, then at most two more individually only if
  needed to find an assistant answer. Otherwise publish the confirmed status. Do not collect the full
  output history or fall back to a full turn read for completion; Codex retains that history.
- Keep raw native evidence for seven days after a closed Run is fully published, then remove it and its current Run
  references. The ticket, final Result and published attachments remain. Unfinished Runs,
  unfinished publications, pending deliveries and retryable scheduler actions retain the local records they need.
  A full ticket may omit the additional automatic result comment; its complete Result and history
  must still publish without leaving a reservation behind.
- Cancellation retains intent and requires confirmed native completion/interruption.
  After cancellation is confirmed, users may return the ticket to Backlog without scheduling it.
  Lost interruption replies remain unknown. An unfinished turn may close as failed after the
  AgentManager confirms the original owned process tree stopped and a replacement epoch started.

## Recovery and contact

- Reconcile unfinished claims/publications before fresh work. Fair bounded scans recover missed
  notifications; notifications only accelerate authoritative reads. Rotate operation, Run, Task
  and delivery scans so scheduling a waiting Task cannot starve its Run. Unchanged blockers do
  not create revisions. Keep reconciliation failures visible during retry backoff until a real
  recovery pass succeeds.
- New workspace/model planning discovers current contracts per attempt; retained native Runs
  continue to reconcile their original request identities and definitions.
- Category lookup uses a disposable, root-scoped projection of Task identity, revision, creation
  time, archive membership and category. Tasks and the durable change journal remain authoritative:
  establish a replay boundary before rebuilding, apply committed writes idempotently and rebuild
  after an event gap or owner change. Ordinary writes must not rescan the complete Task family.
- Comments always remain in the ticket. Only important actionable questions (`purpose=question`,
  `contactUser=true`) request configured Chat/Phone contact. Routine updates do not. Successful
  Review handoff sends one durable completion notification over that route. Main/Voice records
  ordinary answers as ticket comments; native answers use the exact AgentManager request identity.
- Phone falls back to Chat only after a conclusively unstarted or unanswered call.
  Unknown outcomes cannot create another contact. Delivery recovery continues independently of
  Task workflow state.

Checks: [engine](../tests/task-board-engine.test.ts), [scheduler](../tests/task-board-scheduler.test.ts),
[reconciliation](../tests/task-board-reconciler.test.ts), [delivery](../tests/task-board-deliveries.test.ts).
