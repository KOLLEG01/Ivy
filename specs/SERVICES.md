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

[Agent contracts](schemas/agent.schema.json), [implementation](../services/agent-manager/src),
[native environments](AGENT-INSTRUCTIONS.md).

An isolated [Claude adapter option](CLAUDE-ADAPTER.md) uses the same AgentManager service contract.

- Project the exact supported native catalog without renaming methods, dropping fields or
  introducing a provider-neutral protocol. Native templates pin exact schema, definitions
  and immutable parameter revisions; changed defaults cannot redirect saved work.
- Keep native resource ownership and connection epoch explicit. Inventory may expose stored
  tasks that the current connection cannot control. Failed enumeration preserves prior observations.
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

Checks: [operations](../tests/native-operations.test.ts), [inputs](../tests/agent-interactions.test.ts),
[native projection](../tests/native-projection.test.ts), [staged files](../tests/agent-staged-files.test.ts).

## Task and channel services

[TaskBoard](TASK_BOARD.md) owns task workflow and result review.

[ChatBridge](../services/chat-bridge/src) owns one WhatsApp transport and exactly one current
Main binding. Sender admission precedes message persistence. Original message identities prevent
loopbacks and duplicate input; delayed replies retain their channel/context. Service notices enter
Main as retained assignments; only Main's resulting reply reaches the outbox. Consumers discover
the unique Main through Hive and submit only operation ID, source and text. Consumer settings
contain no ChatBridge destination or configuration/tool hash. Original operation receipts remain
readable across native plan and presentation changes; caller, source, input, binding and result
identity checks still apply. Pending service replies are confirmed only after every WhatsApp
output part has a durable transport receipt. Delivery confirms
the original transport result or stays unknown. Transport retention and recovery are tested in
[Chat tests](../tests/chat-outbox.test.ts). `Chat.Settings.language` optionally selects `en` or
`de` for ChatBridge-authored WhatsApp copy; omission uses English. It is a presentation setting,
not part of the workspace `Definition` or the separate transcription-language setting.

ChatBridge observes unresolved Hive warning/error diagnostics. A problem lasting one minute
enters Main once; confirmed resolution produces one recovery notice. Original notice requests
and deduplication state survive restarts in the existing journal. Incomplete diagnostic reads
cannot imply recovery. Short reconnects remain quiet, and failures of the notice mechanism
itself remain visible as a diagnostic without recursively generating notices.
Checks: [operational notices](../tests/chat-diagnostic-notifications.test.ts),
[binding evidence](../tests/hive-tool-binding-diagnostics.test.ts).

[PhoneBridge](PHONE-VOICE.md) owns SIP, audio and its native runtime. Call admission, screening,
routing and shutdown must use the exact principal/peer/call identity. Voice task operations use
the configured Codex CLI and native WebRTC boundary. Actual device/media
readiness is required before reporting an active call. See [call checks](../tests/phone-calls.test.ts).

The [automation example](../docs/examples/services/automation-example/src) demonstrates durable
event consumption, period claims and persistence before ACK; it is source-only example code.
