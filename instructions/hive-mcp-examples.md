# Public MCP workflow examples

These are complete tool argument objects. Replace example IDs, revisions, hashes and timestamps with values from the named read calls. Use the ID format documented by the selected tool. The examples below use Hive operation identities formed from the current `hive_status.runtimeEpoch`, current Unix milliseconds and a unique nonce; PhoneBridge calls require UUIDs. Keep both copies of an outer and inner `operationId` identical for the same routed action. The example IDs illustrate shape; they are not live identities.

## Edit a Wiki page

Call `wiki_read` to obtain the current object ID and revision. Then pass the exact current revision to `wiki_update`. After an uncertain response, read the object and its history before retrying with the same mutation ID.

<!-- mcp-example: wiki_read -->
```json
{"objectId":"page-id"}
```

<!-- mcp-example: wiki_update -->
```json
{"mutationId":"epoch:1760000000000:wiki-edit","objectId":"page-id","expectedRevision":7,"markdown":"# Updated page\nReviewed content."}
```

## Create and advance a TaskBoard task

Select the TaskBoard `serviceNodeId` from the tool schema and read `task_configuration` before creating a ticket. Use its current defaults unless the user explicitly requests different settings. `priority` is 4 for highest and 0 for lowest. `executionRequirement: null` inherits the board default; `{ "kind": "automatic" }` selects any eligible host. Omitted `userContact` inherits the current board default on creation and preserves the ticket setting on edit. `control: "agent"` permits automatic scheduling. Read the task after creation and use its current revision for later changes.

<!-- mcp-example: task_create -->
```json
{"serviceNodeId":"task-board-node","input":{"action":"create","operationId":"epoch:1760000000000:task-create","fields":{"title":"Review the report","description":"Check the report and record findings.","acceptanceCriteria":["Findings are saved on the task."],"category":null,"control":"agent","priority":2,"executionRequirement":null,"workspaceRequirement":{"kind":"task_workspace"},"dependencies":[],"nextReviewAt":null,"dueAt":null}}}
```

<!-- mcp-example: task_update -->
```json
{"serviceNodeId":"task-board-node","input":{"action":"transition","operationId":"epoch:1760000000000:task-ready","taskId":"task-object-id","expectedRevision":3,"workflowState":"todo","detail":null}}
```

User comments normally queue agent work. To record a note without scheduling it, set `agentDelivery` explicitly.

<!-- mcp-example: task_comment -->
```json
{"serviceNodeId":"task-board-node","input":{"action":"comment","operationId":"epoch:1760000000000:task-note","taskId":"task-object-id","expectedRevision":4,"commentId":"comment-id","body":"For the record.","requests":[],"responses":[],"attachments":[],"replyTo":null,"agentDelivery":"none"}}
```

## Read a Codex task through AgentManager

Use direct native Codex tools when they can reach the owning host. For fallback, select the AgentManager service node from the public schema, read `agent_manager_status`, then call `agent_manager_discover` with the exact method. Copy its `nativeVersion` and `expectedDefinitionHash` into `agent_manager_invoke`; use the returned native input schema for `params`. The example is valid for the catalog version shown and the hash must come from the selected host's discovery result. Check `runtimeConstraints` before passing optional native parameters.

<!-- mcp-example: agent_manager_discover -->
```json
{"serviceNodeId":"agent-node","input":{"method":"thread/read"}}
```

<!-- mcp-example: agent_manager_invoke -->
```json
{"serviceNodeId":"agent-node","input":{"operationId":"epoch:1760000000000:native-read","nativeVersion":"0.154.0","method":"thread/read","params":{"threadId":"thread-id","includeTurns":false},"expectedDefinitionHash":"sha256:0000000000000000000000000000000000000000000000000000000000000000"}}
```

## Update a Secretary assignment

Read `secretary_binding`, copy its `expectedScope` into `secretary_list_assignments`, and take the current assignment pin and full value from that result. Change only the intended fields while preserving `assignmentId`, `builtInKey` and `createdAt`. Keep the routed and inner operation IDs equal. A stale pin requires another read.

<!-- mcp-example: secretary_binding -->
```json
{"serviceNodeId":"secretary-node","input":{}}
```

<!-- mcp-example: secretary_list_assignments -->
```json
{"serviceNodeId":"secretary-node","input":{"expectedScope":{"secretaryId":"secretary","rootObjectId":"scope-root"},"assignmentId":"assignment-id"}}
```

<!-- mcp-example: secretary_update_assignment -->
```json
{"serviceNodeId":"secretary-node","operationId":"epoch:1760000000000:assignment-update","input":{"operationId":"epoch:1760000000000:assignment-update","assignment":{"objectId":"assignment-object-id","revision":2},"value":{"schemaVersion":1,"assignmentId":"assignment-id","name":"Review updates","description":"Daily review","enabled":true,"builtInKey":null,"trigger":{"kind":"schedule","timeZone":"Europe/Berlin","cadence":"daily","intervalMinutes":null,"localTime":"09:00","weekdays":[]},"prompt":"Review the latest updates and report meaningful changes.","preflight":null,"rules":{},"createdAt":"2026-09-24T08:00:00.000Z","updatedAt":"2026-09-24T08:00:00.000Z"}}}
```

## Recover an uncertain action

Keep its original operation ID. For TaskBoard, call `task_read` with the ID; for AgentManager, use `agent_manager_read`; for Secretary, call `secretary_operation_read` with the binding's `expectedScope` and the original ID. A Secretary `unknown` result or `not_found` error does not prove that no effect occurred; retry only with the exact original request and ID while it remains valid. Re-read the current object and revision before deciding whether another action is needed. A timeout or accepted phone/chat request does not prove the external effect completed.

<!-- mcp-example: task_read -->
```json
{"serviceNodeId":"task-board-node","input":{"operationId":"epoch:1760000000000:task-create"}}
```

<!-- mcp-example: secretary_operation_read -->
```json
{"serviceNodeId":"secretary-node","input":{"expectedScope":{"secretaryId":"secretary","rootObjectId":"scope-root"},"operationId":"epoch:1760000000000:assignment-update"}}
```
