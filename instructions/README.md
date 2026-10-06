# Runtime instructions

[default-agent-instructions.md](default-agent-instructions.md) is the packaged instruction template
used only by AgentManager when Hive has no active override. Its name deliberately prevents Codex
from loading it as repository guidance. [skills](skills) is the canonical source for skill patches
shipped by AgentManager. The
[generator](../tools/contracts/generate-agent-environment-defaults.mjs) embeds these files in its
defaults; regenerate after edits and use `--check` to detect drift. A Hive-managed AGENTS.md
override replaces the packaged template under the
[agent environment requirements](../specs/AGENT-INSTRUCTIONS.md).

[hive-mcp-instructions.md](hive-mcp-instructions.md) is the full user MCP introduction. Hive
appends registered service discovery hints and workflows to it for MCP `initialize` and `server/discover`;
[ivy-dev-mcp-instructions.md](ivy-dev-mcp-instructions.md) is the separate development MCP introduction.
AgentManager appends only [the short agent summary](hive-mcp-agent-summary.md) and live service
hints to its managed instructions output. Keep the three shared wiki and usage paragraphs identical.

[Local host operations](../specs/DEPLOYMENT.md#local-host-operations) and repository rules
stay in the checkout; AgentManager does not distribute them to native homes.
