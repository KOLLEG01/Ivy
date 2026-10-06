import { consoleBase } from "./runtime";

const protocolVersion = "2026-07-28";
let sequence = 0;

export type JsonObject = Record<string, unknown>;

function record(value: unknown): JsonObject {
  if (value === null || typeof value !== "object" || Array.isArray(value))
    throw new Error("MCP returned an invalid object response.");
  return value as JsonObject;
}

export async function mcpRequest(
  method: string,
  params: JsonObject,
  signal?: AbortSignal,
): Promise<JsonObject> {
  const id = "console-mcp-" + ++sequence;
  const response = await fetch(new URL("mcp", consoleBase), {
    method: "POST",
    credentials: "same-origin",
    ...(signal ? { signal } : {}),
    headers: {
      "Content-Type": "application/json",
      Accept: "application/json, text/event-stream",
      "MCP-Protocol-Version": protocolVersion,
      "Mcp-Method": method,
    },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id,
      method,
      params: {
        ...params,
        _meta: {
          "io.modelcontextprotocol/protocolVersion": protocolVersion,
          "io.modelcontextprotocol/clientInfo": {
            name: "ivy-console-discovery-viewer",
            title: "Ivy Console",
            version: "1",
          },
          "io.modelcontextprotocol/clientCapabilities": {},
        },
      },
    }),
  });
  const body = record(await response.json());
  if (!response.ok || body.error) {
    const error = body.error;
    throw new Error(
      error &&
        typeof error === "object" &&
        !Array.isArray(error) &&
        typeof (error as JsonObject).message === "string"
        ? String((error as JsonObject).message)
        : `MCP request failed (${response.status}).`,
    );
  }
  return record(body.result);
}

export async function allMcpTools(signal?: AbortSignal): Promise<{
  pages: JsonObject[];
  items: unknown[];
}> {
  const pages: JsonObject[] = [],
    items: unknown[] = [],
    cursors = new Set<string>();
  let cursor: string | undefined;
  for (let page = 0; page < 100; page++) {
    const result = await mcpRequest(
      "tools/list",
      cursor ? { cursor } : {},
      signal,
    );
    pages.push(result);
    const values = result.tools;
    if (!Array.isArray(values))
      throw new Error("MCP response is missing tools.");
    items.push(...values);
    const next = result.nextCursor;
    if (next === undefined || next === null) return { pages, items };
    if (typeof next !== "string" || cursors.has(next))
      throw new Error("MCP pagination repeated an invalid cursor.");
    cursors.add(next);
    cursor = next;
  }
  throw new Error("MCP pagination exceeded 100 pages.");
}

export { protocolVersion };
