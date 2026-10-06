/** Hive-owned top-level routes cannot be claimed by a UI. */
export const reservedUiSlugs = [
  "api", "mcp", "mcp-dev", "mcp-icon.svg", "mcp-dev-icon.svg",
  "ui", "console", "login", "logout", "oauth", "health", ".well-known", "app", "app-worker.js",
];

export const uiPath = (metadata: { uiId: string; slug?: string }): string =>
  metadata.slug ? encodeURIComponent(metadata.slug) + "/" : "ui/" + encodeURIComponent(metadata.uiId) + "/";
