# Web UI requirements

Implementation: [shared UI](../packages/ui), [browser helpers](../packages/ui-client),
[Console](../services/hive/console/src), [apps](../ui).
Delivery and authentication follow [Hive](IVYHIVE-SPEC.md).

- Ivy UIs use the shared Vue/TypeScript shadcn component library and SDK. Keep its shared theme
  aligned with the shadcn website. Exact dependencies live in the lockfile; do not copy shared
  components or introduce another component, transport or state authority.
- Compile the common launcher, navigation and theme into each release.
  Apps use independent URLs and preserve deep links, reload and Back/Forward selection.
- All apps share one shell: a permanent desktop app rail (home, installed UIs, System and the
  account menu), a collapsible sidebar with the UI's own navigation, and a top bar holding the
  page title and primary actions, with the current UI's settings at the far right. On mobile
  the rail and page navigation open together as one sheet; settings remain in the top bar.
  A UI without its own navigation shows only the rail; on mobile the rail then names its entries.
  Keep pages quiet: features that are useful only occasionally live in overflow menus, popovers,
  side sheets or settings instead of persistent page chrome.
- Automatically updated views omit general refresh buttons. Errors offer targeted retries;
  settings editors may explicitly reload saved values to discard local changes.
- Controls come from the shared library: selects use the shadcn Select through `OptionSelect`,
  optional detail uses `Disclosure`, and launch or list cards are links over the whole card.
- Keep explicit resource selection and drafts across refresh/conflicts. Reconnect rereads
  authoritative state rather than replaying queued mutations. A changed contract blocks the
  affected action while preserving user input.
- Live views share one SDK WebSocket per app/tab. Each loader opts into its actual change
  source; keep polling for sources without full push coverage. Confirm that source's
  subscription before suspending periodic reads and use one refresh owner per dataset.
  Readiness belongs to acknowledged scopes and provider filters: adding another view must not refresh existing
  views. Load the initial snapshot after acknowledgement, with a bounded HTTP fallback when
  the socket is unavailable; reconnects refresh current state.
  Use polling during connection/setup failure or failed reads. Coalesce
  transient invalidations, recover current state after reconnect or browser suspension, and
  preserve drafts, selection and paging. Empty invalidations are liveness heartbeats, not reads.
  Scope service hints by service and owner, inventory by namespace, kind and owner, and
  object reads by contract. Changes on another owner must not reload a task's native panels.
- Loading, empty, stale, offline, unsupported and failed are distinct states. Show real owner,
  observation time and limits; missing measurements are unknown, not zero.
- All browser UIs share one SDK client with bounded admission: 16 logical requests per client, at most eight
  routed to providers, and 128 waiting. Ordinary reads may pass waiting provider calls.
  Cancellation removes waiting work and the deadline includes its wait; sent work is never
  automatically replayed. Pages reuse recent read bindings and load JSON list documents in
  bounded query pages; residual collection reads use four workers. Hive holds each routed call
  until its provider answers, and tabs sharing a credential share Hive's aggregate budget.
  The SDK gathers read micro-batches and shares concurrent identical observations with
  independent cancellation. Completed responses are not cached. Writes fence earlier
  observations and are sent individually. Keep provider reads separate by provider and
  from ordinary Hive reads so a slow owner cannot block other owners.
  Bound Tools use their pinned `readOnlyHint` to classify reads, including native reads
  that require an operation ID; each call retains that ID and its definition hash.
  Native panels share capability discovery and simultaneous identical reads, including
  independent cancellation. They share a short readiness observation for their selected owner, suppress
  provider reads during known unavailability, and back off failed journal polls while
  retaining automatic recovery and the original owner.
  Native invocation failures retain known Tool bindings; rediscover a binding when its
  definition changes or its cache expires.
- Every UI belongs to one installable Ivy browser app, scoped to the configured Hive base
  path. The shared account menu offers installation and per-browser notification settings.
  A shared desktop/mobile installation banner remains until installation or dismissal,
  remembered per browser profile and Hive base path across visits and tabs. Opening
  installation help alone does not dismiss it; the account menu remains available afterwards.
  Web Push requires HTTPS (or local development), explicit permission and, on iOS/iPadOS,
  a Home Screen installation. The worker does not cache authenticated content.
- Hive sends opted-in browsers agent completion/input notices, TaskBoard review/question/
  handoff notices and actionable Secretary decisions. Stored keys and queued deliveries
  survive restarts; bounded retries handle temporary delivery failures. Logout, revoked
  credentials, opt-out and expired subscriptions stop subsequent delivery. Messages already
  accepted by a browser's push service may still arrive. Notification clicks stay within the
  originating Hive installation and open the relevant UI.
  Services register and publish the shared [browser notice envelope](../packages/sdk/src/browser-notice.ts)
  through their normal event connection; each service owns its notification policy.
- Render external text/Markdown safely, paginate large results and show truncation.
  Agent replies show referenced raster images inline with an enlarged preview. Read local and
  relative image paths through the task's owning native server, for both Codex and Claude;
  decoded image bytes become tab-local blobs. Plain links and code examples never trigger image reads.
  Keyboard focus and semantic labels must remain usable. Every layout and interaction must work
  on both desktop and mobile, including narrow-screen list/detail flows.

## Application responsibilities

| UI         | Requirements                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Console    | Start page lists installed apps in the sidebar and as launch cards. Chat has no web UI. Operational/data views live under System, whose navigation replaces the start page's; Hive settings open from the top bar; Releases belongs only under Runtime. Problems default to unresolved diagnostics. Settings edits complete HostConfigs with revision checks and protected markers. Generic Objects are inspected read-only in sibling order. Retention exposes policy/collection state.                                                                                                                                                                                                                                           |
| AgentUI    | The sidebar shows one task tree across all hosts, one marked section per host with its projects; a host filter only narrows that tree and navigation never changes it. Navigation marks working tasks and results not yet seen in this browser, updated live from native lifecycle events. Use the native Codex project catalog for listing, creation, renaming and removal. Bundle `IvyInternal` tasks in a fixed, initially collapsed group per host, hidden unless this browser chooses to show them, and keep them searchable. Ephemeral native service contexts are not tasks. Allow explicit projectless creation with a permanent task directory and later project reassignment. Discover hosts/projects/tasks, create and continue supported native work, select actual native model capabilities, show results live with consecutive tool steps collapsed into one summary, and answer current-owner pending input. A running task shows a working indicator. Messages sent while a turn runs are queued and follow in order when it ends; each queued message can instead steer the running turn. The composer uses compact pills, keeps Goal and adding photos or files behind +, and stages attached files on the owning host so messages reference them by path (images as local images). Archiving a task that works on a TaskBoard ticket archives the ticket; deletion is confirmed, permanent and unavailable for ticket tasks. Cached visibility does not imply control. Manage versioned agent environments through their owning contracts.                                                                                                                                                                                                                                                                                                |
| TaskBoardUI | Open the sole registered TaskBoard directly. With multiple registrations, use a dropdown picker in the top bar without an intermediate selection page. The board is the only page, without page navigation: the Backlog sits below the work board, followed by a collapsed Archive in the same list format that is fetched only when opened. The Backlog is ranked by hand (drag, or move to top/up/down/bottom) and has its own swimlane setting. Description, acceptance criteria and comments use the shared WYSIWYG editor; pasted, dropped or added images and files become Task attachments and images show inline. Show ticket, execution condition, control, category, priority, requirements, comments and attachments. Results, questions and native request controls share the comment timeline. Agent work configuration exposes the selected native model, reasoning effort and speed/service tier from actual native capabilities. Provide TaskBoard settings for default host, model, reasoning, speed, updates route, dedicated worktree and parallel project work; the editor preselects these values for new tickets. Models that some eligible hosts lack remain visible as unavailable. Agent work always names one host: the default host is preselected and marked invalid when it lacks a required capability, and other hosts that lack one cannot be chosen. The project picker offers no project, the host's projects, a browsed host folder or a new project. Host and project are editable until the first run and read-only afterwards; the UI offers no reassign. Shared projects without worktrees run sequentially unless both tickets allow parallel work. Short fixed choices use chips, categories a creatable combobox, and blockers a task picker that never offers a cycle. Tickets open in a side sheet or a linkable full page with bordered comments, a field-by-field revision history and a link to the agent task. Ticket edits appear as a small unbordered timeline event and still notify an agent when needed. Offer only valid revision-checked actions, including Move to Done and Move to Backlog through the same transition as the board. Comments on Done show an unchecked Move to Todo checkbox. |
| WikiUI     | Browse/search the Markdown hierarchy, drag pages into, beside or out of other pages, and edit from the latest read revision with autosave: changes are saved shortly after typing stops, drafts stay in the browser until the save is confirmed, and a conflict keeps the draft for review. Images keep their place while pages refresh and can be selected and removed. Retain drafts on conflict, and expose ID links, attachments, history and diff without implicit writes when viewing history.                                                                                                                                                                                                                                                                                                                                                                                |
| Dashboards | List dashboards in the sidebar and as cards; view one full-size with frameless link, PNG export and delete in an overflow menu; edit title, HTML, data sources and refresh beside a live draft preview. `#/view` stays the frameless display. |
| DataCollector | List tasks with live run state in the sidebar; show a task's status, actionable error guidance, schedule and selectable result revisions; edit every task setting as a form, with only configuration and module lists as validated JSON. |

Console must remain useful without registered apps/providers. A failed request cannot erase other
navigation/views. Host reports are cached observations; build/recovery stays with host tools.
HostConfig secrets use the [same protected projection](DEPLOYMENT.md) in Settings and generic reads.

AgentUI shows resolved native model, working mode, reasoning effort and permission defaults in both composers. Capability reads recover independently and retain successful options during refresh. Hive settings store separate Codex and Claude profiles at `/ivy-agent-execution-defaults`; null fields inherit native configuration and profiles apply to new work. Completion notices open the complete conversation; explicit turn links show a scope banner only when other turns are excluded.

`IvyInternal` tasks produce no browser notices, unread badges or activity-triggered inventory reads. Determine membership from native project IDs, falling back to containing project roots only when membership is absent. Keep native lifecycle events and pending inputs available to their owners and manually opened conversations.

Native actions show waiting on their submit button and reconcile the original receipt automatically without replaying mutations. Keep routine pending and successful receipts out of the conversation. Confirmed failures appear immediately; after two minutes, distinguish work still in progress from an unconfirmed result with concise feedback. Retain the draft and operation identity across reload, and offer a retry only after the original owner confirms absence.

Display the native failure message returned to the original caller. Refresh a retained terminal failure once when opening its view; checking its receipt never resends the action. TaskBoard action notices can be dismissed after a confirmed terminal outcome without changing the original workflow receipt.

Checks: [UI content](../tests/ui-content.test.ts), [browser workflows](../tests/web-e2e).
