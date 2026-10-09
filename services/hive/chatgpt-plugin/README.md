# Ivy in ChatGPT

The PNG assets in `assets/` are used by the Ivy plugin in ChatGPT. They are
rasterized from `../src/mcp-branding.ts`; the live MCP endpoints serve SVG icons
independently of the plugin package.

Hive Settings includes a setup guide and generates a ZIP after the user enters
the registered OpenAI app ID or ChatGPT plugin detail URL. The package generator
is in [`console/src/openai-plugin.ts`](../console/src/openai-plugin.ts); increment
its package version when changing the exported package. The ZIP keeps
`.codex-plugin/plugin.json` at the root for **Upload new version** in ChatGPT.
Deployment-specific app IDs and downloaded packages stay outside Git.
