import { readFileSync } from "node:fs";

export const ivyDevMcpInstructions = readFileSync(
  new URL("./ivy-dev-mcp-instructions.md", import.meta.url),
  "utf8",
).trim();
