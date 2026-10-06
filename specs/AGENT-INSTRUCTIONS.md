# Native homes and agent environments

Contracts: [agent schema](schemas/agent.schema.json).
Implementation: [environment](../services/agent-manager/src/environment.ts),
[instructions](../services/agent-manager/src/instructions.ts), [daemon](../services/agent-manager/src/daemon.ts).
Defaults: [runtime instructions](../instructions/README.md).

- The connected Codex instance is the sole project authority. Read `project/list` and
  use native project mutations; host configuration and directory allocations never
  add project inventory entries. A successful empty list replaces the previous
  snapshot. Saved plans reference native project IDs and must reject deleted entries.

- Persistent service tasks and standard TaskBoard tasks use the host's shared internal
  project root as their working directory. Keep task-specific files in subdirectories
  without changing that working directory. Explicit project, directory and worktree
  selections remain authoritative; use the native project containing the selected path.
  Resolve membership before retaining native start parameters; recovery reuses the
  original request. A missing internal project must prevent an unassigned task.
  See [project selection](../packages/sdk/src/native-project.ts). Native project IDs
  are authoritative; Desktop display requires its supported membership synchronization.

- Use the exact supported native version and explicitly selected connection/home. Shared-daemon
  shutdown must leave the external daemon alive; owned stdio is an explicit alternative.
  No silent mode change, native update or Desktop restart is permitted.
- Shared native homes, history and credentials remain external user data, including during
  backup/restore. Instance-owned homes require explicit owned-stdio configuration.
- Status polls and fresh resume calls must avoid returning conversation contents. Read
  complete output on demand through native item pages, retaining the one-turn fallback
  when the first item page is explicitly unsupported. Preserve original saved requests
  and evidence; metadata-only resume still continues the same native conversation.
  See [turn contents](../packages/sdk/src/native-turn-contents.ts).
- New AgentManager instances default to `<ivyRoot>/codex/<instanceId>`, with skills
  in that home's `skills` directory. Windows uses an external proxy to the managed
  daemon; Linux uses owned stdio. Explicit homes, skills roots and connection modes
  take precedence. Native executables, hashes and account sign-in remain local inputs.
- Compose guidance deterministically with host overrides taking precedence over Hive guidance.
  Apply only to the verified native home, preserve the original override and never modify
  the user's AGENTS.md. Inline nonempty home AGENTS.md content because Codex selects the
  home override first. In the managed AGENTS.override.md, place `## Ivy MCP`
  immediately before the four-paragraph Ivy MCP introduction and live service capability
  summary, after active local, Hive and host guidance. Keep the remaining general guidance
  and service-specific `Tool workflows:` section in on-demand `ivy_instructions`.
  Put @AGENTS.md last when local guidance is enabled. A disabled
  instruction document still suppresses the
  managed output.
- Instruction, MCP and skill documents are separately versioned. Project-owned defaults apply
  before first launch; `instructions/default-agent-instructions.md` is an AgentManager template,
  not repository guidance, and remains the Hive-wide fallback until an explicit Hive AGENTS.md
  override replaces it. Host inheritance and disabled/removed values keep their schema-defined
  meanings.
- `settings.patcherEnabled` controls AgentManager's native-home instruction, MCP and skill
  patching for one host. It defaults to enabled when omitted. Disabling it leaves existing
  local files untouched and stops further patches; re-enabling resumes managed reconciliation.
- Apply atomically with bounded recovery. Local edits, malformed markers, duplicate unmanaged
  MCP names and another instruction authority are conflicts. Read failures retain the last
  applied output; no update promises that an already loaded task refreshes its context.
- Codex configuration projection owns one marked TOML block after unmanaged root keys and before
  unmanaged tables. Existing two-block installations are migrated with their edit checks. It selects the
  V1 permission behavior (`approval_policy = "never"` and
  `default_permissions = ":danger-full-access"`), disables the synchronized hosted Ivy app while
  direct Ivy MCP is managed, and injects the provisioned credential only for explicitly selected
  AgentManager authentication at this installation's Ivy MCP URLs. Legacy sandbox settings
  and unmanaged entries for the owned keys are conflicts. Keep secrets out of stored environment
  documents, UI responses and logs; preserve all other unmanaged configuration.
- Managed skill patches use their declared names as directories under the selected account skills root,
  beside existing skills. Their ownership state is anchored beside that root and migrated from the former
  native-home location. Active patches in older `ivy-managed-*` directories are migrated to their plain
  names after checking ownership and local edits. Validate paths, links and sizes before replacing a
  current patch. If a patch is omitted later, leave its directory in place and release ownership;
  unrelated directories remain external data. On startup an existing skill state waits for the
  Hive override read before reconciliation, so packaged defaults cannot temporarily release
  host-specific patches.
- Ship runtime skills only. Host operations belong to the repository deployment specification.

Checks: [instructions](../tests/agent-instructions.test.ts), [environments](../tests/agent-environment.test.ts).
