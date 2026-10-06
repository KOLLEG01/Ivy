# Ivy in ChatGPT

The PNG assets in `assets/` are used by the Ivy plugin in ChatGPT. They are
rasterized from `../src/mcp-branding.ts`; the live MCP endpoints serve SVG icons
independently of the plugin package.

Keep the installed plugin package, its app ID and its version outside Git. To
update the existing installation, copy the assets into that package, increment
its manifest version, zip the package contents so `.codex-plugin/plugin.json`
is at the ZIP root, then use **Upload new version** in ChatGPT.
