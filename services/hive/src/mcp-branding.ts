import type { McpSurface } from "./mcp.js";

const symbol =
  "M20 18C43 18 61 33 64 54C67 33 85 18 108 18C108 43 95 61 74 69V110H54V69C33 61 20 43 20 18Z";

export function mcpIconSvg(surface: McpSurface): string {
  const development = surface === "ivy_dev";
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128" role="img" aria-label="${development ? "Ivy Development" : "Ivy"}"><rect width="128" height="128" rx="26" fill="#1f2937"/><path d="${symbol}" fill="#fff"/>${development ? '<circle cx="105" cy="105" r="16" fill="#f59e0b" stroke="#fff" stroke-width="4"/>' : ""}</svg>`;
}

export function mcpIconPath(surface: McpSurface): string {
  return surface === "ivy_dev" ? "/mcp-dev-icon.svg" : "/mcp-icon.svg";
}
