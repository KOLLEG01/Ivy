# Service requirements

Service lifecycle: [SDK service client](../packages/sdk/src/service.ts).
Wire fields and operations: [schemas](schemas).

## Shared lifecycle

- A service has an explicit instance identity, its own configuration and process owner.
  Reconnect must authenticate, synchronize its registry and reconcile state before readiness.
  Shutdown cancels reconnect and bounded active work.
- Readiness includes required native dependencies. Hive-dependent work stops on connection
  loss; accepted external work retains its original identity for reconciliation.
- Long actions retain original arguments and operation identity before effects and expose
  status. Request correlation IDs do not replace durable operation IDs.
- Cached read bindings refresh once after a confirmed pre-execution definition rejection,
  including reads invoked through `call`, retaining their provider, interface, arguments
  and operation identity. Mutations still require their exact
  retained definition and domain recovery; a replacement read must remain read-only.
- Hive retains rejected obsolete bindings as operational diagnostics until the same caller
  completes a call with the current contract. Evidence contains routing identities, never
  request arguments or credentials.
- External messages, calls and writes require the originating user's authorization.
  Tool annotations, source content and recovery do not grant new authority.

## AgentManager

[Agent contracts](schemas/agent.schema.json), [implementation](../services/agent-manager/rust/src),
[native environments](AGENT-INSTRUCTIONS.md).

An isolated [Claude adapter option](CLAUDE-ADAPTER.md) uses the same AgentManager service contract.

- Project the exact supported native catalog without renaming methods, dropping fields or
  introducing a provider-neutral protocol. Native templates pin exact schema, definitions
  and immutable parameter revisions; changed defaults cannot redirect saved work.
- Keep native resource ownership and connection epoch explicit. Inventory may expose stored
  tasks that the current connection cannot control. Failed enumeration preserves prior observations.
- Expired observations release active request capacity without terminating accepted turns.
  Late replies remain recognizable with bounded correlation state; explicit unresolved request
  identities stay fenced and cannot be dispatched again as a timeout recovery.
- Durable invocation saves intent before dispatch and outcome before acknowledgement. Direct
  interactions and read observations retain their distinct recovery contracts; losing a
  transient interaction cache does not authorize replay.
- Completion contact belongs to the workflow service that started the exact native turn.
  AgentManager suppresses duplicate browser completion notices for registered service callers;
  direct user turns and actionable native inputs retain their normal notices.
- Pending input is bound to its original owner, epoch and native request shape. Old requests
  expire on native restart. Only the catalog-defined clock request is answered automatically.
- Native failure may require a new owner generation; Hive reconnect alone must not restart
  native work. No hidden prompt rewriting, Desktop patching or fallback to another owner.
- `agent.stageFile` stores a message attachment of up to 8 MiB by content on its host and
  returns its path for native local-image or file references; unused files expire after 30 days.

Checks: [operations](../tests/rust/agent-service-kernel.rs), [inputs](../tests/rust/agent-interactions.rs),
[native projection](../tests/native-projection.test.ts), [staged files](../tests/rust/agent-staged-files.rs).

## Task and channel services

[TaskBoard](TASK-BOARD.md) owns task workflow and result review.

[ChatBridge](../services/chat-bridge/rust/src) owns one WhatsApp transport and exactly one current
Main binding. Sender admission precedes message persistence. Original message identities prevent
loopbacks and duplicate input; delayed replies retain their channel/context. Service notices enter
Main as retained assignments; only Main's resulting reply reaches the outbox. Consumers discover
the unique Main through Hive and submit only operation ID, source and text. Consumer settings
contain no ChatBridge destination or configuration/tool hash. Original operation receipts remain
readable across native plan and presentation changes; caller, source, input, binding and result
identity checks still apply. Pending service replies are confirmed only after every WhatsApp
output part has a durable transport receipt. Delivery confirms
the original transport result or stays unknown. Transport retention and recovery are tested in
[Chat tests](../tests/rust/chat-outbox.rs). `Chat.Settings.language` optionally selects `en` or
`de` for ChatBridge-authored WhatsApp copy; omission uses English. It is a presentation setting,
not part of the workspace `Definition` or the separate transcription-language setting.

Explicit file delivery uses `chat_bridge_send_file` and the existing WhatsApp connection.
Read `chat_bridge_read` with `{fileTarget:true}`, then submit its exact bridge/binding,
one selected file's canonical base64 bytes, filename, MIME, byte length and SHA-256,
an optional caption and a stable operation ID. Local paths and arbitrary download URLs
are not accepted. PNG/JPEG use native image messages; other admitted formats retain their
original bytes as named documents. MIME signatures and UTF-8 text are checked; unknown
binary formats require `application/octet-stream`. Limits and request shapes are in the
[Chat schema](schemas/chat.schema.json). Read `{fileOperationId:operationId}` for the
original delivery: queued/sending does not prove delivery; sent requires WhatsApp server
acknowledgement. Uncertain media sends are never repeated automatically. File intent and
bytes share the ordered durable transport journal. A changed Main, account or recipient
blocks dispatch. The file queue admits 32 unresolved deliveries and 64 MiB, expires files
after seven days and retains at most 256 completed receipts with compact replay fences.
Checks: [file delivery](../tests/rust/chat-files.rs).

`Chat.Settings.diagnosticNotifications` is an optional boolean, defaulting to `false`.
It controls automatic diagnostic warning/failure and recovery notices. Disabling it
preserves notices already admitted to Main and their delivery receipts.
When explicitly enabled, ChatBridge observes unresolved Hive warning/error diagnostics. A problem lasting five minutes
enters Main once; resolution must remain stable for five minutes before one recovery notice.
Diagnostics with the same visible service, error code and dependent service form one incident,
independently of internal resource IDs. Recovery requires the entire group to remain healthy.
Hive's shared quiet hours govern operational notices without depending on an optional service.
Quiet periods retain current failures without queuing resolved incidents or recoveries for later
delivery. WhatsApp also gates notice replies queued before quiet hours, while explicit user
replies can progress. Paused notices keep their original message IDs and receipts. Requests
and deduplication state survive restarts in the existing journal. Incomplete diagnostic reads
cannot imply recovery. Combining incidents preserves all unfinished original delivery receipts.
Short reconnects remain quiet, and failures of the notice mechanism
itself remain visible as a diagnostic without recursively generating notices.
Receive receipts save immutable reply snapshots before changing delivery state. If retention
prunes an offered reply revision, recovery verifies its current immutable content against that
original snapshot and preserves the original offer and acknowledgement identities.
Checks: [operational notices](../tests/rust/chat-diagnostics.rs),
[binding evidence](../tests/rust/hive_network.rs).

[PhoneBridge](PHONE-VOICE.md) owns SIP, audio and its native runtime. Call admission, screening,
routing and shutdown must use the exact principal/peer/call identity. Voice task operations use
the configured Codex CLI and native WebRTC boundary. Actual device/media
readiness is required before reporting an active call. See [call checks](../tests/rust/phone_call_commands.rs).

The [automation example](../docs/examples/services/automation-example/src) demonstrates durable
event consumption, period claims and persistence before ACK; it is source-only example code.
