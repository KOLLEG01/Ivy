# Hive requirements

Contracts: [operations](schemas/operations.json), [wire schema](schemas/hive-wire.schema.json),
[operation schema](schemas/hive-operations.schema.json).
[Interface boundaries](INTERFACE-BOUNDARIES.md) apply to every transport.

## Access and storage

- Use the configured canonical base URL for links, cookies and redirects, including subpath
  installations. Do not derive authority from arbitrary forwarded headers.
- Non-browser clients authenticate explicitly. Browser sessions use secure HTTP-only cookies;
  mutations and WebSocket handshakes require the exact origin. Browser code never stores credentials.
- Sessions survive package updates in persistent Hive storage. Logout ends the browser session;
  credential revocation invalidates its authenticated access. OAuth codes/tokens remain bound
  to their client, principal, resource and PKCE flow.
- Registered UIs are trusted code; external content is inert. Enforce CSP, MIME and path
  boundaries. Public health probes reveal no inventory or secrets.
- Unsupported storage formats fail before readiness. SQLite transactions own consistency;
  services and UI code never open the Hive database.

Sources: [server](../services/hive/src/server.ts), [OAuth](../services/hive/src/oauth.ts),
[store](../services/hive/src/store.ts), [storage schema](../services/hive/src/storage-schema.ts).

## Contracts and Objects

- A contract family has a fixed owner/media type and immutable exact versions. New definitions
  do not migrate Objects or redirect writes. Writes name an exact version, never `latest`.
- Objects have stable IDs, navigation metadata and immutable content revisions. Content changes
  use CAS; contract versions cannot decrease. Historical reads use the saved contract.
- Content, an optional new name, current revision, search projection, mutation receipt and core event commit atomically.
  Replaying an identical retained mutation returns its original result; changed arguments conflict.
  [Retention](RETENTION.md) defines expiry and reference protection.
- Parent identity determines hierarchy; paths are derived. Names are normalized, sibling-unique
  and cannot create cycles. A bounded optional icon decorates navigation; a dense position orders
  siblings and can be changed only within the same parent. Queries can select
  `object.hasContractChildren` so navigation omits expand controls for empty branches. Content CAS does not protect independent
  navigation changes.
- Ancestor archival affects listings without rewriting descendants; explicit child archive state
  survives unarchiving a parent. Cross-links use IDs and exact revisions where required.
- Permanent deletion is limited to Wiki pages, Wiki attachments, TaskBoard tasks and dashboards. It requires the current
  revision, removes all selected Objects and their revisions in one transaction, and rejects
  external exact references or published assets. Page deletion includes descendants and owned
  attachments; task deletion also includes its linked workflow Objects. A Task with an active
  claim or publication cannot be deleted. The operation is available to the UIs via RPC but
  is neither discoverable nor callable through MCP.
- Multi-Object workflows require owner reconciliation; Hive is not a domain job engine.

Sources: [Objects](../services/hive/src/objects.ts), [contract admission](../services/hive/src/registry.ts).

## Reads and routing

- Queries are bounded, typed and parameterized. Null/missing values, scalar comparisons and
  literal substring matching follow the [query implementation](../services/hive/src/query.ts);
  no caller SQL, regex or implicit structured-value stringification is accepted.
  JSON queries may include complete documents at the selected revisions; their bytes count
  toward the same page bound and the cursor binds this option.
- Cursors bind the complete query and deterministic order. Pagination reads current state,
  not a cross-request snapshot; truncation and retention gaps must be explicit. A continuation
  must advance or fail, never return an endless empty page.
- A new connection fences its predecessor. Live routing requires the current authenticated
  generation, successful atomic registry sync and readiness. Cached inventory/catalogs remain
  timestamped observations and never prove a live route.
  `ready` is the service's reported readiness; `stale` separately marks an old heartbeat
  observation. Observation age alone does not revoke a connected, ready route.
- Complete inventory snapshots replace prior state atomically; incomplete reads cannot erase it.
  Conflicting selectors or multiple eligible owners fail rather than selecting a substitute.
- Errors distinguish refusal, completion and unknown external outcome. A dispatched timeout or
  disconnect ends waiting, not execution; it never authorizes automatic mutation replay.
  Request reservation refusals identify the exhausted budget, scope, active count and limit without
  exposing consumer identities. Aggregate consumer capacity is separate from per-connection
  capacity; retained-byte bounds remain independent of request counts.
- The `ivy` MCP endpoint publishes user-facing direct tools; `ivy_dev` publishes separate
  development and maintenance tools with its own instructions. Services opt in per tool
  through `discovery.mcp.name`, prefixed by the service-declared MCP prefix. Several
  distinct operations of one namespace may share a name when their schemas identify
  exactly one operation; otherwise the facade requires a `view` selector for the
  overlapping variants. Omitted publication stays internal. Hive exposes no generic MCP
  tool search, describe or dispatch wrappers.
- Direct service tools select an exact provider and carry typed domain input; routing binds
  the current definition and preserves the original operation identity. Schemas and guides
  remain provider-specific when hosts differ. An unavailable owner is never substituted.
- Published tools carry compact action titles and descriptions, complete input schemas,
  and output schemas when they return data. Parameterless tools retain MCP's required
  empty-object input schema; null-only and empty-object results have no output schema
  or structured payload. Service guides live in server instructions. Prefer
  Wiki page tools and service functions; generic objects and schemas cover data without
  a dedicated operation. Internal SDK discovery remains available independently of MCP.
  Prefer native Codex functions when they can reach the host; AgentManager owns discovery
  and hash-bound fallback access for the selected host's version-dependent native methods.
  Each tool shares identical schema definitions without merging different provider
  contracts. Routing hashes and internal binding metadata stay on the server.
- Each endpoint paginates only its own tools within the discovery response size limit.
  Catalog changes invalidate cursors, and `tools/call` rejects tools from the other endpoint.
- Both endpoints require OAuth at the HTTP boundary, with protected-resource discovery
  and authentication challenges; tools omit repeated security schemes and default visibility.
  Open-world hints describe external reach, not remote hosting. Explicit provider hints
  take precedence over known tool metadata; unknown tools retain conservative MCP defaults.
  Additive operations are non-destructive; updates and arbitrary agent actions retain
  destructive hints. Idempotency requires repeat calls to preserve the original effect;
  a short-lived response cache alone does not provide that guarantee.
- Service roots prefer their registered `discoveryHint`, falling back to namespace descriptions.
  Tool descriptions explain the action; namespace guides give the entry point, necessary
  resource discovery and recovery rules. Service-specific search terms belong in its registry.
  Detail schemas include all reachable definitions, without unrelated definitions or truncation.

Sources: [routing](../services/hive/src/live-routing.ts), [inventory](../services/hive/src/inventory.ts),
[discovery](../services/hive/src/discovery.ts), [error contract](../packages/contracts/src/errors.ts).

## Events and apps

- All subscriptions persist their filter and acknowledged cursor. ACK follows successful
  consumer persistence and must match an issued batch boundary; consumers tolerate duplicates.
  Reconnect resumes from the durable cursor. Event hints carry no durable delivery authority.
- The `durable` flag adds retention protection; Secretary may set it on its subscriptions.
  The existing Hive event journal
  keeps every event after the oldest such acknowledgement, even past normal age and count limits.
  When 10,000 events remain unprunable, Hive refuses ordinary new events with a retryable capacity error;
  Secretary has a bounded reserve of 1,024 object-change events to save executions and advance
  its cursor. Once acknowledged events are pruned, publishers can retry. A pruned gap that
  predates protection remains visible to the subscriber.
- Service registries describe subscribable topics with a stable topic/version pair, payload
  schema, display text and optional event kinds. `topics.list` exposes the admitted catalog with
  its exact provider so user interfaces can attach durable subscriptions without service-specific
  knowledge. Display metadata may change; topic identity and payload schema remain immutable.
- A registry may carry an optional `discoveryHint`. Hive composes hints from MCP-publishing services into the
  file-backed MCP instructions returned by `initialize`, `server/discover` and
  `discovery.instructions`; AgentManager appends only the four-paragraph Ivy MCP
  introduction and live service hints to active managed instructions. The remaining
  guidance and service-specific workflows remain available through `ivy_instructions`.
  Hints do not add tools, permissions or execution authority.
- Releases pin complete immutable assets and portable backend requirements. Validate paths,
  hashes, entry assets and retained data compatibility before atomic selection. Installation may
  precede backend availability; explicit inspection also reports current compatibility. Launch
  checks protocol and current provider interfaces/readiness; consumers check returned record versions.
  Application catalogs return installed metadata and release pointers without inspecting data
  history or release files.
- Rollback selects a compatible retained release without reverting data. Immutable URLs never
  serve changed bytes; [retention](RETENTION.md) determines how long a release remains available.
- UI metadata may declare a unique `slug` for a stable `/{slug}/` URL; the internal `uiId`
  and existing `/ui/{uiId}/` links remain valid. Hive's API, MCP, authentication, health,
  Console and discovery paths are reserved. Bare slug URLs redirect to the trailing slash
  so relative assets resolve correctly. Optional integer `priority` orders navigation and
  catalog pages highest first (default 0), with `uiId` breaking ties.
  Stable UI URLs serve the selected release and revalidate
  cached responses with ETags; explicit release asset requests remain immutable. Browser
  navigation to a versioned `index.html` returns to the stable URL.
- Console belongs to the Hive artifact and works without registered apps/providers.
  Host deployment reports are observations; the host journal remains authoritative.

Sources: [events](../services/hive/src/events.ts), [apps](../services/hive/src/uis.ts),
[transport checks](../tests/transports.test.ts), [storage checks](../tests/storage.test.ts).
