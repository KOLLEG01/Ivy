# Offline environment import

Stop the owners of the selected snapshot and destination before running
`ivy-agent-offline-import`. Supply one JSON object on stdin:

| `kind` | Other required fields |
| --- | --- |
| `skills` | `sourceState`, `root`, `hostId`, `hiveUrl` |
| `mcp` | `home`, `hostId`, `hiveUrl`, `credential`, `document`, `openai` |

Paths must be absolute for the executing platform. Select the source state,
destination, authority and document snapshots explicitly. The command writes one
`result` or `error` JSON object; credentials enter through stdin. Request bounds
and exact validation live in [offline_import.rs](../services/agent-manager/rust/src/offline_import.rs).
The existing `ivy-agent-mcp-import` command retains its six-field input without
`kind`.

Skills imports require the matching packaged locale libraries and license beside
the executable. An absent or incompatible locale runtime returns
`locale_unavailable` before import. MCP imports run independently of that payload.

Imports retain the original source bytes and trees, including a byte-for-byte
state backup when replacing state in place. Version 1 Skills trees are copied to
current names; existing current-format imports can be repeated when their state
and trees still match. Local edits and unowned destinations cause a refusal.
Inspect an interrupted import before rerunning it. Service startup does not
perform these imports or recover interrupted imports automatically.
