# TaskBoard requirements

Contracts: [schema](schemas/task-board.schema.json), [operations](schemas/task-board.operations.json).
Implementation: [runtime](../services/task-board/src/runtime).
UI requirements: [UI](UI.md).

## Work and allocation

- A Task is the durable ticket; Runs are execution attempts. Its stable task key also identifies
  per-task output directories. Workflow state, execution condition and user/agent control are separate.
  Category is organizational metadata, not a capability or execution target.
- Creation stores its selected workflow state directly; omission means Todo for every caller.
  Explicit Backlog preserves unscheduled preparation. Waiting requires public deferral details
  and uses `fields.nextReviewAt` for time waits; linked blockers retain their idle/done distinction.
  Manual In Progress creates no native execution. Manual Review requires an initial final Result;
  initial final material may also accompany manual Done. Agent In Progress and Review require
  their real native claim/completion workflows and cannot be fabricated by creation. Done and
  Cancelled may record an already closed task without inventing a Run or acceptance.
  Task, initial manual Result and history publish through one retained Create operation;
  an unfinished publication cannot schedule work. Offline host/capability waits retain valid
  registered selections and defer unavailable live model validation until allocation.
- Tickets retain the latest 50 revisions. Each ticket has one retained `task-board/comments`
  child with current-only revisions; immutable comment bodies are stored there once. Ticket
  snapshots select the conversation prefix and preserve mutable delivery, native-input and
  handoff observations. Reads, including historical reads, restore that exact comment view.
  Comment read markers independently retain the all-agent prefix and direct-agent count;
  markCommentsRead with includeCrossAgent=false advances only direct-agent visibility.
  Task reads report both unread totals; hiding directed communication never acknowledges it.
  Storage backfill also covers archived tickets without changing workflow state and retries
  active or conflicting tickets on later passes. Before readiness enables the new policy,
  current-only migration ledgers protect source revisions of legacy unfinished Runs without
  changing their claims or accepted operation pins. Periodic refresh releases obsolete ledger
  references; new Runs retain their sources directly. Other Hive families keep their
  own retention policies.
- Permanent deletion removes the Task, its comments child, attachments, all Hive revisions, and task-linked
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
  grace period. Without a usable reset time, retry after fifteen minutes. Confirmed model-capacity
  failures retry after one minute. Continue with the same model and context; repeated temporary
  failures defer again. General failures, cancellation and unanswered questions require intervention.
  When the condition clears, the scheduler rechecks time, dependencies, host and workspace before
  continuing. User-controlled work and Backlog remain unscheduled. A task waiting for a host
  names why each AgentManager was skipped. Manual rank in any board column is the ticket's Hive
  sibling order; it orders the board only, and scheduling still follows priority.
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
- Delegated tasks identify their original task with `parentTaskId`; their separate hosts and worktrees
  do not change that responsibility. The parent reviews saved final results and integration, moves
  accepted children to Done, and archives them. Waiting on a child uses `until=idle` so its Review can
  be assessed. Named deliveries (`until=delivery`, `key`) wait for exact published artifact or code
  references without requiring the whole child to reach Done. Each idle/done or named delivery
  identity wakes the same work once; repeating an unchanged condition does not create a polling loop.
  Host capacity, memory admission and execution retry belong to the executing agent system.
  Coordination-only turns may accept and archive children while preserving the parent's
  result and blocker. Delegation cycles are rejected. Omitted child contact defaults to ticket;
  completion is delivered once to the parent through normal directed-comment handling.
- Agents publish one final comment with `purpose=handoff`, useful output links, actual verification
  and material limitations. Its structured `handoff` declares overall readiness (`workflowState=review`)
  or remaining work (`waiting` with public waiting details), and `nextReviewAt` or null.
  TaskBoard applies this intent with the result only after confirmed native completion; merely ending
  a successful turn does not establish readiness. Missing intent displays workflow attention separately
  from an unanswered user question. Completion first reconciles accepted handoff operations under
  their original Run identity. Only unchanged work under the same claim permits rebasing an
  unpublished handoff across unrelated ticket revisions.
  Only actual review readiness sends the configured root completion notification or delivers the delegated result to its parent. Unanswered questions and failed attempts remain
  Blocked; queued follow-up work returns to Todo unless an explicit blocker remains. Users move root Review to Done using the normal board
  transition, with no result-acceptance action or extra review state.
- Ordinary questions remain open until an addressed user reply (`replyTo`), their typed answers,
  or explicit `resolvesQuestions` with an explanation. Unrelated replies and handoffs never close them.
  Native questions retain their owner-bound answer workflow. Open questions remain visible during checks.
  A `questionWait` names the exact ticket and question comment blocking another task; an empty local
  question count does not answer it. Its addressed answer permits work to resume.
- A handoff or idle deferral may add `checkScope`, an explicit independent check performed at
  `nextReviewAt`. TaskBoard schedules it in Blocked or Review despite unanswered ordinary questions,
  while still enforcing dependencies, host/workspace availability, active claims and unknown outcomes.
  The retained check-only Run acknowledges field-edit markers already in its pinned snapshot,
  but does not consume queued discussion, steer into general work, replace a
  result awaiting Review or repeat its completion notification. New discussion waits for a normal Run.
  Each check explicitly renews its future date/scope or clears it. Resolved native failures or missing
  check intent retain the scope, publish a visible failure and retry after fifteen minutes (or the
  native reset time), preserving a confirmed later date or an explicit timer stop.
  Owner-reported `checkFailure` likewise stays visible until a successful check.
  Scheduling belongs to TaskBoard; no second scheduling assignment is required. Durable claims and
  original operation reconciliation prevent duplicate starts across restarts and missed notifications.
- Serialize publication per Task until the first Task write has durably retained the owning
  Operation marker. Root coordination additionally covers shared category and dependency graph
  changes. That marker and Task CAS then protect the unfinished publication while up to twenty
  independent Tasks reconcile concurrently. Free slots continue reconciling while another Task
  awaits its owner; scans retain bounded pending pages. An ambiguous first write retains its Task gate until
  exact replay proves the marker; recovery must finish the original prepared writes. Dependency
  checks include committed marked intents, and optional worktree reservations remain unique by
  host and canonical path. Shared checkouts serialize active work unless both tasks allow parallel execution.
  Failed publication recovery yields before retrying so unrelated Tasks can progress. Advisory native
  signals retain only their current checkpoint; immutable execution receipts and evidence remain separate.
- Final comments retain actual verification, limitations and artifact/Git links. Internal Run outcome
  records remain available for recovery; they are not a second user-facing result or acceptance workflow.
  Unavailable evidence is not success; local commits are not represented as confirmed remote commits.
- Completion requires the exact native turn's terminal status and uses its ticket handoff comment.
  Without a handoff, read the newest output item first, then at most two more individually only if
  needed to find an assistant answer. Otherwise publish the confirmed status. Do not collect the full
  output history or fall back to a full turn read for completion; Codex retains that history.
- Raw native evidence and completed operation payloads enter the bounded local FIFO history
  after a closed Run is fully published, without a minimum retention duration. Compact replay
  guards prevent reuse of evicted operation IDs. Compact caller-bound receipts retain original request
  hashes and outcomes after payload eviction, without permitting replay. The ticket, final Result and published attachments remain. Unfinished Runs,
  unfinished publications, pending deliveries and retryable scheduler actions retain the local records they need.
  A full ticket may omit the additional automatic result comment; its complete Result and history
  must still publish without leaving a reservation behind.
- Cancellation retains intent and requires confirmed native completion/interruption.
  After cancellation is confirmed, users may return the ticket to Backlog without scheduling it.
  Lost interruption replies remain unknown. An unfinished turn may close as failed after the
  AgentManager confirms the original owned process tree stopped and a replacement epoch started.

## Recovery and contact

- Execution mode is one of work, coordination, independent check or completion recovery.
  Admission and scheduling share mode selection; the owner validates mode-specific writes.
  Missing handoffs take priority over peer coordination, without consuming queued discussion.
  Recover the original accepted publication first. After confirmed native termination, a
  recovery-only continuation reuses the same context to publish existing work, never to repeat
  implementation or uncertain effects. Its original Run and necessary evidence remain protected.
  A legacy pre-closure pin already pruned before protection may use the same original Run's
  confirmed terminal revision; this never authorizes replay or a replacement Run identity.
  Two completed unusable recovery attempts require inspection; transport backoff does not reset
  the durable attempt count. Explicit stops, user control and genuine questions remain binding.
  An explicit move to Todo rearms the bounded handoff recovery in the existing context,
  or clears a terminal execution error so the original work can be retried.
- A handoff may select Todo with a concrete `nextStep` and a null review date to continue
  already authorized work. Waiting details and check scopes are forbidden on this disposition.
  Unchanged committed/output evidence may continue twice before a visible no-progress fault.
  Technical failed or recovery outcomes do not replace a previously saved work result.
  Operational recovery failures remain visible independently of service readiness.

- Reconcile unfinished claims/publications before fresh work. Fair bounded scans recover missed
  claims whose Run is terminal through their original completion operation; age alone never clears
  a claim. Scans also recover missed
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
- Ticket-only contact sends no browser push notices. Delegated handoffs notify their parent,
  not the user; root tasks retain their configured completion contact.
- Phone falls back to Chat only after a conclusively unstarted or unanswered call.
  Unknown outcomes cannot create another contact. Delivery recovery continues independently of
  Task workflow state.

Checks: [engine](../tests/task-board-engine.test.ts), [scheduler](../tests/task-board-scheduler.test.ts),
[reconciliation](../tests/task-board-reconciler.test.ts), [delivery](../tests/task-board-deliveries.test.ts).
