# TaskBoard requirements

Contracts: [schema](schemas/task-board.schema.json), [operations](schemas/task-board.operations.json).
Implementation: [runtime](../services/task-board/rust/src).
UI requirements: [UI](UI.md).

## Native runtime

The existing journal and borrowed generation graph live on one dedicated thread inside
the service process. SDK I/O, host control and heartbeat publication stay on the host
runtime. [Public admission](../services/task-board/rust/src/service_bridge.rs) bounds
held calls and retained argument/reply capacity; losing an observer does not cancel
accepted work. Retirement drains that work before releasing the journal.
The generation uses one borrowed job queue for category, execution, reconciliation
and internal invocation work. [Shared admission](../services/task-board/rust/src/work_budget.rs)
covers retained public/internal arguments, unread replies and action record caches.
Cache admission includes unused vector capacity and takes original records;
readers copy only the fields their API returns. Independent read scopes retain
separate immutable revision maps. Comment-node observers reuse unchanged write-pin
hints. Shared JSON encoding writes control and unpaired UTF-16 escapes from fixed
storage while preserving canonical bytes and size/error ordering.
Internal replies transfer
their original buffers to one observer instead of copying a completed value.
[Generation control](../services/task-board/rust/src/generation.rs) coalesces readiness,
wakeups and compact native identity hints. Hint overload retires the generation and
reconciles original durable work after drain. Readiness reads a bounded owner-observed
snapshot without waiting on SQLite; expired observations degrade until refreshed.
Only the latest pending connection can replace a drained generation.

Board card pages use `list` with `view:cards`, Hive predicates and stable ordering,
at most 20 rows and the original opaque cursor. The owner first selects immutable
Task pins, then projects bounded bulk header pages through the existing card reader.
Changed or oversized headers use exact selected revision reads. The browser receives
compact cards directly, without a separate card cache or per-card provider calls.
Board counters consume Hive's bounded SQL counts grouped by workflow and category;
the browser collects numeric groups rather than individual Tasks. Waiting and
category cards use the same bounded pages and opaque continuations. Group counts
and empty cells describe the current page; overall totals remain SQL counts.
Cards and counts subscribe to their direct workspace parent, so writes in another
workspace do not refresh them. Broader navigation hints and owner availability
still invalidate the affected views; visibility and burst coalescing remain shared.

## Export maintenance

Stop the owning TaskBoard instance, then run its packaged `ivy-task-board-maintenance`
with `--config ABSOLUTE_PATH --plan ABSOLUTE_PATH --stopped-pid PID`.
The [export plan and job records](../services/task-board/rust/src/maintenance/records.rs)
retain the exact host, service, scope, hashes, revision pins and local archive paths.
The operator verifies the local export before retiring references; `--comments`
admits a verified generated-comment archive job. Startup resumes both cleanup jobs.
Ordinary comments, delivery receipts, unexported files and live Run source pins remain protected.

## Work and allocation

- Execution choices retain the complete backend, provider and account identity. Equal
  model names do not merge models or options across accounts. Model, reasoning and
  speed defaults apply only within the same account; changing account clears those
  overrides and existing project selection. A retained conversation keeps its account.
  Project IDs and directory reads use that same owner. A scoped project name requires
  a current account observation; retained inventory alone cannot prove account ownership.

- Model, reasoning and speed remain editable during execution. Save the desired settings
  and request that the current turn yield at a safe stopping point through the existing
  steering channel. After confirmed completion, continue the same conversation with the
  latest settings; consume the pending marker with the durable start claim and reuse normal
  retries. The yield does not publish a final result or notify the parent/user. Cancellation
  wins. Idle settings edits do not wake tickets or invalidate their results.

- Questions explicitly address `user` or a task ID. Task questions remain canonical in the
  sender ticket and are relayed once through durable comments to the recipient. An answer
  names that recipient in `sourceTaskId` and the original question in `replyTo`. Internal
  questions never produce user question badges or contact notifications. The agent decides
  whether an answer permits further work. Escalating to a human
  requires a separate explicit user question.
- Failed completion/publication is an execution error with its retained cause and a central
  Hive error diagnostic. A storage refusal is not a missing handoff and does not trigger
  repeated agent recovery attempts.

- A Task is the durable ticket; Runs are execution attempts. Its stable task key also identifies
  per-task output directories. Workflow state, execution condition and user/agent control are separate.
  Category is organizational metadata, not a capability or execution target.
- Creation stores its selected workflow state directly; omission means Todo for every caller.
  Explicit Backlog preserves unscheduled preparation. Waiting resumes on external comments, watched ticket changes or an optional `wakeAt` appointment.
  An idle agent wait without an open question, watched ticket or appointment is an execution error.
  Retry it after thirty seconds until the agent continues work, submits for review or records a real wait.
  This recovery does not restart manual work, Backlog, Review, terminal tickets or unresolved executions.
  Manual In Progress creates no native execution. Manual Review requires an initial final Result;
  initial final material may also accompany manual Done. Agent In Progress and Review require
  their real native claim/completion workflows and cannot be fabricated by creation. Done and
  Cancelled may record an already closed task without inventing a Run or acceptance.
  Task, initial manual Result and history publish through one retained Create operation;
  an unfinished publication cannot schedule work. Offline host/capability waits retain valid
  registered selections and defer unavailable live model validation until allocation.
- Tickets retain the latest 50 revisions. Each ticket pins an immutable conversation tree.
  `task-board/comment-node` children store individual comments with delivery observations;
  64-way index pages preserve exact historical snapshots with bounded range reads. Unchanged
  records are reused; prepared publications retain the compact root rather than copying the
  entire conversation into every journal stage. Local `prepared-payload` records capture the
  target contract version, compact content and exact Hive references before application.
  Replay uses those saved bytes; domain reads can materialize the original conversation.
  Existing inline prepared records preserve their original reference construction on replay;
  conversation logs remain readable.
  Header edits retain the same root and evidence pins without reading conversation entries.
  Generated edit notices append through the right index path and retain historical roots;
  ordinary appends use the same path and update only affected open-question summaries.
  A bounded disposable local identity index is valid only for its exact task/root;
  a missing, incomplete or oversized index streams bounded ranges and proves no absence early.
  Comment admission still owns identity, request and reply validation. Worker handoff
  admission and publication read compact headers while retaining the original work/Run
  binding and revalidating the current claim before reservation.
  Native publications read compact headers and only their admitted input receipts.
  Successive receipt changes use the root from the exact saved reservation, including recovery
  from an older retained publication. Completion selects the latest handoff and addressed-reply
  presence by Run from that same bounded index. Its explicit delta retains original input
  receipts, appends at most one generated result or adds delivery to the selected handoff;
  existing comment bodies and unrelated receipts remain immutable. Appends update affected
  question summaries; an agent result does not implicitly resolve an ordinary user question.
  Terminal reconciliation selects that bound handoff before reading final output and refreshes
  the current header before admission, preserving the original primary and archive policy.
  Receipt changes preserve the original comment, sequence and delivery operation, replacing
  only affected immutable leaves and shared ancestor paths. Repeated historical IDs select
  every matching leaf; question summaries and unrelated evidence references remain intact.
  Active native reconciliation reads current and original headers plus queued receipt IDs
  from that same bounded, exact-root index. Receipt updates maintain their sequence order
  and repeated IDs; an incomplete projection still reads all bounded ranges before deciding.
  Native call guards and request preparation use the same primary Task-header reader;
  prompts obtain question presence from the bound conversation summary. Legacy records
  keep their full reader and original prompt semantics when that summary is unavailable.
  Scheduler input selection preserves receipt order and checks actual comment eligibility;
  routing selects open questions and uses the same exact-root identity index for relay deduplication.
  An unavailable question recipient does not prevent independent sender scheduling or other
  relays in the bounded batch. The question remains open; a bounded routing diagnostic retains
  its source, question and recipient identities without restoring or starting the recipient.
  Contact delivery selects its original comment from its retained publication, including
  comments without execution receipts. Selected records never replace a Task's full conversation.
  Native-context recovery filters actual open-question records by their original Run;
  a compact question count cannot authorize replacing or restoring a saved context.
  Native input mirroring uses compact headers and the same bounded exact-root index
  to select all native questions for its Run, including closed historical questions.
  Fresh claim/route checks and exact original input records still govern responses.
  Public `read` defaults to the latest 50 comments, newest first. `comments.limit` selects
  1–100 entries; passing `commentPage.nextBefore` as `comments.before` loads older entries
  without shifting the page when comments arrive. A null cursor ends the history.
  `comments.questionsOnly` pages unresolved questions; explicit offset reads remain ascending.
  Full summary construction and fallback question pages retain the resolver's exact ordered
  question records and sequences; a repeated historical ID does not reopen a closed record.
  Reusing a question from the selected page requires both its original ID and sequence.
  Structured responses update every affected open question, including historical reuse
  of a request ID; first and last request positions bound the candidate question range.
  Cards use a compact header summary and do not hydrate history or calculate unread totals.
  The UI loads older comments on scroll, preserves the visible position, and keeps unresolved
  user questions available outside the loaded page. It neither displays nor writes unread state.
  Sending reads the current ticket revision without loading its history. A successful operation
  receipt confirms its comment even outside the loaded page and releases the composer.
  Confirmed writes also release publication gates from older UI snapshots, so a multi-file
  selection continues without waiting for the view to refresh.
  An absent receipt resumes the exact saved request under its original identity; it never
  creates a replacement message. The mutation validates its caller/workspace binding itself.
  Active retained headers reuse bounded in-memory summaries until normal storage backfill.
  Storage backfill also covers archived tickets without changing workflow state and retries
  active or conflicting tickets on later passes. Before readiness enables the new policy,
  current-only migration ledgers protect source revisions of legacy unfinished Runs without
  changing their claims or accepted operation pins. Periodic refresh releases obsolete ledger
  references; new Runs retain their sources directly. Other Hive families keep their
  own retention policies.
- Exact evidence references are deduplicated across attachments, Results and Runs. If a ticket
  exceeds one Hive revision's reference budget, ticket-owned reference pages preserve every
  exact pin. New reference pages use immutable content identities, including before the first
  ticket publication. Historical ticket revisions retain their exact page revisions; storage limits
  must not block publication of an already admitted native completion.
- Permanent deletion removes the Task, its comments child, attachments, all Hive revisions, and task-linked
  Run, Result, Review, History, Delivery and comment-read records. It is refused while a claim
  or publication is active, or another Task still depends on it. The deleted task key is not reused.
- Archiving a ticket also archives its linked Codex tasks, including previous Run and History contexts.
  The same retained archive operation removes `outputs/<task-key>/` on its execution hosts
  after native execution is resolved. Host outages and incomplete deletion keep the operation
  pending for retry. Cleanup only addresses this deterministic output directory; shared projects
  and worktrees remain intact. Store lasting deliverables in their destination or as ticket
  attachments before archiving. Restoring a ticket does not restore deleted working outputs.
  Restoration tries the current native task first; confirmed failure creates a new native task
  while retaining the ticket's workflow state. Pending or unknown native outcomes never create
  replacements. Lifecycle dispatch, archival history, membership and context changes read
  compact headers with their original archive policy. Context and route publications preserve
  the immutable conversation and evidence pins; archived route edits retain their archive CAS.
  Shared archive controls and MCP use the TaskBoard lifecycle action.
  Direct-turn discovery, parent validation, workspace peers and read markers use compact
  headers. A cached discovery checks the bound queued-input projection only when needed;
  canonical source questions are selected from that same immutable root. Manual results,
  reviews, cancellations and reassignment preserve the conversation through explicit
  header changes or generated-notice appends.
  Start, continuation and deferral publish header changes. Comment-triggered admission
  checks only selected original entries and queued receipts; wake checks reuse the
  bounded exact-root input projection. A worker deferral appends its completion intent
  without applying that intent before native completion or closing ordinary questions.
    Question recipient corrections change only selected original entries and their tree paths.
    The exact-root reply index updates open questions and direct counts, including earlier
    replies and repeated historical IDs, while retaining original text, order and receipts.
  Done cards offer Archive task; the Done column offers Archive all tasks for the complete
  filtered column, including cancelled tickets and tasks beyond the current page.
- Explicit `newThread` work creates a new Codex task and queues the existing ticket in Todo.
  Previous contexts remain linked until ticket archival. It requires resolved execution; ordinary continuation reuses the
  current native task. Ticket content, comments, attachments and previous Runs remain available.
- User-controlled task work never starts autonomously. Agent-controlled Todo requests work;
  admission requires an available host and workspace. A missing
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
  admissions, publications and native outcomes. Retain workspace files until explicit archival;
  only the ticket's disposable output directory is removed then.
  Agents discover peers through `task_list relatedTo`, matching the allocated canonical working
  directory and host rather than the project ID. Directed messages and dependencies may cross
  workspaces. Unrelated peer state changes do not inject turns or require board-wide polling.
  Agents coordinate overlapping changes,
  commits and deployments through directed ticket comments; unrelated changes must be preserved.
  Waiting resumes when comments arrive, watched tickets change or the next appointment is due.
  Watches are notifications and may be mutual; they do not gate execution. Backlog suspends
  automatic work. Completion preserves newer appointments and assignments.
  Confirmed native usage-limit failures defer agent work until the reported reset, with a one-minute
  grace period. Without a usable reset time, retry after fifteen minutes. Confirmed model-capacity,
  rate and transient connection failures retry after one minute, including rejected turn starts.
  A terminal interruption may refine to the same turn's actual failure. Cancellation and permanent
  authorization errors never trigger automatic retries. Continue with the same model and context; repeated temporary
  failures defer again, including the original conversation comment batch. General failures and
  cancellation require intervention. Questions remain visible until answered or resolved by the
  agent; ordinary comments can supply the answer. Before starting, the scheduler checks host and workspace before
  continuing. User-controlled work and Backlog remain unscheduled. A task waiting for a host
  names why each AgentManager was skipped. Manual rank in any board column is the ticket's Hive
  sibling order; it orders the board only, and scheduling still follows priority.
- There is no global limit on concurrent Runs. Workspace reservations and native outcome
  reconciliation still prevent conflicting or duplicate execution. After `scheduler.stallWarningMs` (default five
  minutes) without observed native progress, diagnostics warn while preserving the original
  claim. Native activity clears the warning; explicit user-input waits do not count as stalls.
- Metadata edits preserve Review and Done. Changing an agent assignment reopens those states
  in Todo. Manual Done may retain intermediate material; only Review
  requires a final Result. Subsequent comments and edits remain valid after manual closure.
- The agent performs Git lifecycle work from the ticket's explicit workspace/worktree intent.
  TaskBoard and AgentManager do not silently clone, switch branches or alter repositories.
  Completed project and worktree directories are retained; ticket output retention follows archival.

Sources: [allocation](../services/task-board/rust/src/scheduler.rs),
[workspace resolution](../services/agent-manager/rust/src),
[conditions](../services/task-board/rust/src/condition.rs).

## Execution and conversation

- Actions use stable operation identities and expected revisions. One CAS claim authorizes
  an attempt; timeouts or expired leases cannot steal work that may already be executing.
  Publication gates cover the affected ticket, shared index or shared workspace; unrelated
  tickets proceed independently. Local publication contention becomes retryable after 250 ms;
  unavailable owners and uncertain native outcomes retain their longer recovery backoff.
- Retain the Run and native start intent before dispatch. Save the returned native identity
  before a turn; uncertainty never creates a replacement thread/turn.
- Before continuing a saved native context, restore it if it was archived outside TaskBoard.
  Retain and reconcile the unarchive operation before admitting the next Run; preserve the
  thread identity, owner, workspace and queued input. A failed pre-turn resume confirmed as
  archived may retry this path automatically. Active writers and unknown outcomes never
  authorize replacement contexts or duplicate turns.
- Resume and pre-turn reattachment request metadata without hydrating the complete history;
  retain every prepared request unchanged and use paginated reads for turn inspection.
  A confirmed attachment that was unloaded may resume again in the same owner epoch;
  the original prepared turn is preserved and still requires fresh input capability with
  no active turn. Explicit input capability also permits resumed `notLoaded` metadata;
  absent or false capability requires attachment instead of dispatch.
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
  Claimed recovery and result publication stay In progress; actual native input requests wait for their owner-bound response.
- Initial starts, scheduled wake-ups and comment updates use the same concise English prompt
  identifying the Task and owning service through Ivy MCP. It asks the agent to read the ticket
  and decide what to do; it never assigns a next-step ID or repeats the general workflow guide.
  Agents reread the current ticket/settings/comments at every turn, work in the assigned workspace,
  follow the shared writing guidance in `registryText.guideMarkdown` in the
  [TaskBoard runtime inputs](../services/task-board/rust/src/runtime-inputs.json),
  and the [attachment guidance](../instructions/task-board.ts).
- Comments are the single conversation and handoff surface. Ordinary decisions need no separate
  approval workflow. Native requests retain their original identities in comments; UI buttons and
  MCP answer them through AgentManager. Responses are mirrored into the same timeline; secret
  answers stay at the native owner.
- User comments and directed task messages deliver the same short ticket-update prompt.
  Active turns receive steering; idle contexts receive a new turn in the same thread.
  A committed message wakes its ticket immediately without waiting for the periodic board
  scan. Coalesce concurrent changes per ticket and dispatch a prepared steer in the same
  pass. Durable scans recover missed wake-ups after restart; unavailable owners retain
  their backoff. Available owners should receive updates promptly; delivery does not
  depend on the size of the board or use a separate periodic polling loop.
  The agent reads the ticket and decides whether to reply, continue work or wait in that turn,
  including on blocked, manual and terminal tickets. There is no conversation-only mode.
  Every active turn is In progress. A reply needs no handoff and restores the prior idle
  state and result after completion. A declared work handoff applies
  its actual outcome after confirmed completion. Active assignment fields remain fixed.
  A new message re-arms an availability retry once; a failed attempt retains technical backoff.
  Continuing an existing conversation verifies its native identity, working directory and project
  against its retained Run instead of reallocating a workspace through complete project inventory.
- Update prompts contain only the task key, service reference and a request to reread the ticket;
  never copy ticket fields or comment bodies. Running turns receive `turn/steer` with their exact
  expected turn ID. Retained operation receipts acknowledge the original comment batch only.
  Unknown receipts are reconciled before another delivery; a failed steer leaves comments queued
  for retry against the same active turn or the same primary's next turn. Event wakeups and periodic reconciliation share this path.
- Ordinary agent updates never wake their author. Messages with sourceTaskId queue delivery
  by default, just like user comments; agentDelivery=none explicitly records without waking.
- Agents own planning and coordination from the current ticket. `wakeAt` is the next optional
  UTC appointment; `dependencies` contains watched ticket IDs. Their material changes wake the
  agent without prerequisite predicates or a structured step plan. Runtime claim and receipt
  churn does not wake dependent tickets. Archive and deletion count as changes.
  Multiple future appointments belong in ordinary ticket context; renew the next appointment
  after its attempt. Omitted scheduling fields preserve the current values.
- Questions remain local and open until an addressed answer or explained resolution. Cards with
  pending questions addressed to the user have a yellow border. Those questions stay above the composer.
- Keep verification and handoff detail in the ticket according to the shared writing guidance.
  Repeated observations explicitly publish their next wakeAt; unrelated comments never shift it.
- Delegated tasks retain parentTaskId. Parents review exact child results at idle, accept and archive
  completed children. Saved results wake watching tasks before overall completion.
- A confirmed native completion applies an optional handoff workflowState. Only overall Review
  readiness notifies the user. Without a handoff, active work continues; replies to waiting or
  reviewed tickets preserve that state and result. Absence of a handoff is not an error.
- `handoff.nativeContext=release` explicitly declares that no remaining work or afterwork
  needs volatile tool, browser or REPL state. Omission or `retain` preserves the subscription.
  A handoff containing only `nativeContext` preserves the prior workflow, result and acceptance;
  scheduling fields require an explicit workflow state.
  After confirmed completion and durable result publication, the existing Run records a compact
  caller-scoped release intent. Current Task/Run CAS, queued input and the original native epoch
  guard dispatch; accepted release operations retain their original receipts through recovery.
  Comments remain writable. New native work waits only until the owner accepts that original
  release or TaskBoard conclusively retains the context. Provider ordering then preserves foreign
  subscribers and a later continuation of the same saved thread. Release neither archives nor
  deletes a thread, and actual unloading follows the provider's native idle policy.
- Serialize publication per Task until the first Task write has durably retained the owning
  Operation marker. Root coordination additionally covers shared category indexes and execution admission. That marker and Task CAS then protect the unfinished publication while up to twenty
  independent Tasks reconcile concurrently. Free slots continue reconciling while another Task
  awaits its owner; scans retain bounded pending pages. An ambiguous first write retains its Task gate until
  exact replay proves the marker; recovery must finish the original prepared writes. Optional worktree reservations remain unique by
  host and canonical path. Shared checkouts serialize active work unless both tasks allow parallel execution.
  Failed publication recovery yields before retrying so unrelated Tasks can progress. Advisory native
  signals retain only their current checkpoint; immutable execution receipts and evidence remain separate.
  New ticket revisions do not replace a pending scheduler action; finish its original admission first.
- A work outcome has one handoff comment; user notices summarize it and refer back to the ticket.
  Exact proposed public text remains available when approval depends on it. Internal Run outcome
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

- `migrateNativeRoute` moves one native owner's current addresses on an idle ticket
  to a selected backend on the same physical host. An exact terminal Run and retained
  storage provenance identify that owner and durable thread. The selected
  storage must return the same inactive thread, workspace paths and project ID.
  Plan parameters stay unchanged; an explicit provider/account may use a newer
  checked catalog. When the original call survives, moving an unscoped owner across
  native engine identities also requires its exact successful storage receipt. If
  pinned domain evidence exists, both receipts must agree. Missing or uncertain
  receipts cannot establish this storage's provider/account provenance.
  After normal collection of the exact local call, `originalThreadOperationId`
  can identify a retained receipt in the selected storage. The receipt must match
  the original caller, native owner, checked catalog, Run epoch and durable
  thread. A supplied ID must agree with any surviving local call. Read failures
  other than confirmed absence remain errors; no local record is reconstructed.
  When the terminal Run's call graph has been collected and no receipt ID is
  supplied, the selected manager's persisted storage identity must name the Run's
  original service and the selected provider/account. This read does not seed
  provenance. The same fresh durable-thread and workspace checks still apply.
  Claims, pending publications and unresolved original outcomes
  prevent transfer. Archived tickets keep their archive state, fenced in every
  retained Task write; no native context is restored for the move.
  The bounded `nativeRoutes` mapping points each original owner directly to its
  current destination. Historical owners may move independently; moving the current
  primary requires its exact latest Run. History records the transfer. Complete
  original Runs, requests, hashes, native owners, calls, epochs, results and evidence
  remain unchanged. An original receipt never authorizes dispatch through another owner.
- User and directed task comments wake the same agent turn. Waiting questions do not gate them.
  A reconnect can rebind a still-active exact thread/turn after fresh owner evidence, unless the
  previous owned process is confirmed stopped. A rejected steer is retried with a successor
  identity only after its original receipt is conclusively failed. Unknown outcomes keep their
  original identity until reconciled.
- Comments, attachments and native projections rebase on the latest Task under its publication
  gate. Original operation bytes and receipts remain immutable. Assignment changes retain CAS
  protection; unrelated runtime revisions do not invalidate allocation.
- Optional handoffs select todo, waiting or review. Scheduling intent is persisted with the
  handoff comment; later user edits survive native completion. Technical failures preserve saved
  results and remain visible with their retry time when retryable.

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

Checks: [engine](../tests/fixtures/task-board-engine-actions.rs), [scheduler](../tests/fixtures/task-board-scheduler.rs),
[reconciliation](../tests/fixtures/task-board-reconciler.rs), [delivery](../tests/fixtures/task-board-deliveries.rs).
