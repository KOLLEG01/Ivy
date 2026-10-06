import type { IncomingMessage, ServerResponse } from "node:http";
import { randomUUID } from "node:crypto";
import {
  createMcpHandler,
  INVALID_PARAMS,
  METHOD_NOT_FOUND,
  ProtocolError,
  Server,
} from "@modelcontextprotocol/server";
import type {
  CallToolResult,
  ListToolsResult,
  Tool,
} from "@modelcontextprotocol/server";
import { toNodeHandler } from "@modelcontextprotocol/node";
import type { NodeIncomingMessageLike } from "@modelcontextprotocol/node";
import { hashJson } from "../../../packages/contracts/src/canonical.js";
import { mcpDiscoveryResultBytes } from "../../../packages/contracts/src/limits.js";
import {
  IvyError,
  requireThat,
} from "../../../packages/contracts/src/errors.js";
import type {
  Operation,
  Wire,
} from "../../../packages/contracts/src/generated.js";
import type { ConnectionContext } from "./kernel.js";
import type { Outcome } from "../../../packages/contracts/src/errors.js";
import {
  directCall,
  directDescriptors,
  hasStructuredOutput,
} from "./mcp-catalog.js";
import type { McpBinding } from "./discovery.js";
import { hiveMcpInstructions } from "../../../instructions/hive-mcp.js";
import { ivyDevMcpInstructions } from "../../../instructions/ivy-dev-mcp.js";
import { mcpIconPath } from "./mcp-branding.js";

export type McpSurface = "ivy" | "ivy_dev";

export function mcpSurfaceBindings(
  bindings: McpBinding[],
  surface: McpSurface,
): McpBinding[] {
  const publicNames = new Set(
    bindings
      .filter((binding) => binding.definition.discovery?.mcp?.surface === "ivy")
      .map((binding) => binding.definition.discovery!.mcp!.name!),
  );
  return bindings.filter((binding) => {
    const mcp = binding.definition.discovery?.mcp;
    const declared = mcp?.surface ?? "ivy_dev";
    return (
      declared === surface &&
      (surface !== "ivy_dev" || !publicNames.has(mcp!.name!))
    );
  });
}

type CursorAccess = {
  create: (identity: Wire.Json, offset: number) => Promise<string>;
  read: (identity: Wire.Json, cursor: string) => Promise<number>;
};
export interface McpCallObservation {
  requestId: string | number | null;
  internalRequestIds: string[];
  tool: string;
  code: string;
  outcome: Outcome;
  latencyMs: number;
  operationIdentity?: string;
}
type McpOptions = {
  surface: McpSurface;
  version: string;
  publicBaseUrl: string;
  resourceMetadataUrl: string;
  cursors: CursorAccess;
  catalog: (name?: string) => Promise<McpBinding[]>;
  observeCall?: (observation: McpCallObservation) => void;
};

function mcpServerInfo(options: McpOptions) {
  const development = options.surface === "ivy_dev";
  const base = new URL(options.publicBaseUrl);
  return {
    name: options.surface,
    title: development ? "Ivy Development" : "Ivy",
    version: options.version,
    description: development
      ? "Development and maintenance tools for Ivy."
      : "Personal back office with a wiki and connected services.",
    websiteUrl: base.href,
    icons: [
      {
        src: `${base.origin}${base.pathname.replace(/\/$/, "")}${mcpIconPath(options.surface)}`,
        mimeType: "image/svg+xml",
        sizes: ["any"],
      },
    ],
  };
}
function measuredListResult(
  result: ListToolsResult,
  serverInfo: ReturnType<typeof mcpServerInfo>,
): unknown {
  return {
    ...result,
    ttlMs: 0,
    cacheScope: "private",
    _meta: {
      "io.modelcontextprotocol/serverInfo": serverInfo,
    },
  };
}

export function mcpSuccess(result: unknown, structured: boolean): CallToolResult {
  if (!structured) return { content: [] };
  const envelope = { result };
  // Services can return the same PNG envelope over RPC and as native MCP media.
  const png = result as {
    mimeType?: unknown;
    encoding?: unknown;
    data?: unknown;
  } | null;
  if (
    png?.mimeType === "image/png" &&
    png.encoding === "base64" &&
    typeof png.data === "string" &&
    png.data.length <= 12 * 1024 * 1024 &&
    png.data.startsWith("iVBORw0KGgo") &&
    /^[A-Za-z0-9+/]+={0,2}$/.test(png.data)
  ) {
    const { data, ...metadata } = png;
    return {
      content: [
        { type: "text", text: JSON.stringify({ result: metadata }) },
        { type: "image", mimeType: "image/png", data },
      ],
      structuredContent: { result: metadata },
    };
  }
  return {
    content: [{ type: "text", text: JSON.stringify(envelope) }],
    structuredContent: envelope,
  };
}

function failure(error: unknown, resourceMetadataUrl: string): CallToolResult {
  const ivy = IvyError.from(error),
    wire = ivy.toWire(),
    envelope = { error: wire };
  const challenge = `Bearer resource_metadata="${resourceMetadataUrl}", error="invalid_token", error_description="Authentication expired or is no longer valid"`;
  return {
    isError: true,
    content: [{ type: "text", text: JSON.stringify(envelope) }],
    structuredContent: envelope,
    ...(ivy.code === "unauthenticated"
      ? { _meta: { "mcp/www_authenticate": [challenge] } }
      : {}),
  };
}

export async function handleMcp(
  request: IncomingMessage,
  response: ServerResponse,
  body: unknown,
  context: ConnectionContext,
  invoke: (context: ConnectionContext, request: unknown) => Promise<unknown>,
  options: McpOptions,
): Promise<void> {
  const requestIdValue = (body as { id?: unknown } | null)?.id;
  const requestId =
    typeof requestIdValue === "number" && Number.isSafeInteger(requestIdValue)
      ? requestIdValue
      : typeof requestIdValue === "string"
        ? requestIdValue.slice(0, 128)
        : null;
  const rpc = (method: string, params: unknown) =>
    invoke(context, { jsonrpc: "2.0", id: randomUUID(), method, params });
  const method = (body as { method?: unknown } | null)?.method;
  const instructions =
    options.surface === "ivy_dev"
      ? ivyDevMcpInstructions
      : method === "initialize" || method === "server/discover"
        ? (
            (await rpc(
              "discovery.instructions",
              {},
            )) as Operation.DiscoveryInstructionsResult
          ).instructions
        : hiveMcpInstructions;
  const serverInfo = mcpServerInfo(options);
  const handler = createMcpHandler(
    () => {
      const server = new Server(serverInfo, {
        capabilities: { tools: {} },
        instructions,
        cacheHints: {
          "server/discover": { ttlMs: 0, cacheScope: "private" },
          "tools/list": { ttlMs: 0, cacheScope: "private" },
        },
      });
      server.setRequestHandler("tools/list", async (listRequest) => {
        try {
          const bindings = mcpSurfaceBindings(
            await options.catalog(),
            options.surface,
          );
          const descriptors = directDescriptors(bindings);
          const catalogHash = hashJson(descriptors);
          const identity = {
            method: "tools/list",
            surface: options.surface,
            catalog: catalogHash,
          } as Wire.Json;
          const requestedCursor = listRequest.params?.cursor;
          const offset = requestedCursor
            ? await options.cursors.read(identity, requestedCursor)
            : 0;
          requireThat(
            offset >= 0 && offset <= descriptors.length,
            "invalid_cursor",
            "MCP discovery cursor does not match the current catalog.",
          );
          const tools: Tool[] = [];
          // The largest offset has the longest cursor. Measure the fixed reply
          // once, then add each descriptor's serialized size only once.
          const largestCursor = offset < descriptors.length
            ? await options.cursors.create(identity, descriptors.length)
            : undefined;
          const fixedWithCursor = Buffer.byteLength(JSON.stringify(measuredListResult({ tools: [], nextCursor: largestCursor }, serverInfo)));
          const fixedWithoutCursor = Buffer.byteLength(JSON.stringify(measuredListResult({ tools: [] }, serverInfo)));
          let toolBytes = 0;
          for (let index = offset; index < descriptors.length; index++) {
            const candidateOffset = index + 1;
            const candidateBytes = toolBytes + (tools.length ? 1 : 0) + Buffer.byteLength(JSON.stringify(descriptors[index]!));
            if ((candidateOffset < descriptors.length ? fixedWithCursor : fixedWithoutCursor) + candidateBytes > mcpDiscoveryResultBytes) break;
            tools.push(descriptors[index]!);
            toolBytes = candidateBytes;
          }
          requireThat(
            tools.length || offset === descriptors.length,
            "result_too_large",
            "One MCP descriptor exceeds the discovery page budget.",
          );
          const nextOffset = offset + tools.length;
          const nextCursor = nextOffset < descriptors.length ? await options.cursors.create(identity, nextOffset) : undefined;
          return { tools, ...(nextCursor ? { nextCursor } : {}) };
        } catch (error) {
          const failure = IvyError.from(error);
          throw new ProtocolError(INVALID_PARAMS, failure.message, {
            code: failure.code,
          });
        }
      });
      server.setRequestHandler("tools/call", async (call) => {
        const args = call.params.arguments ?? {};
        const startedAt = Date.now(),
          internalRequestIds: string[] = [];
        const operationValue =
          args["operationId"] ??
          args["mutationId"] ??
          (args["input"] as Record<string, unknown> | undefined)?.[
            "operationId"
          ];
        const operationIdentity =
          typeof operationValue === "string"
            ? operationValue.slice(0, 512)
            : undefined;
        let code = "internal_error",
          outcome: Outcome = "unknown";
        const callRpc = (method: string, params: unknown) => {
          const id = randomUUID();
          internalRequestIds.push(id);
          return invoke(context, { jsonrpc: "2.0", id, method, params });
        };
        try {
          const bindings = mcpSurfaceBindings(
            await options.catalog(call.params.name),
            options.surface,
          );
          if (!bindings.length) {
            code = "method_not_found";
            outcome = "not_executed";
            throw new ProtocolError(
              METHOD_NOT_FOUND,
              "Unknown Hive MCP tool.",
              { tool: call.params.name.slice(0, 128) },
            );
          }
          const result = mcpSuccess(
            await callRpc("discovery.call", directCall(bindings, args)),
            hasStructuredOutput(bindings),
          );
          code = "ok";
          outcome = "completed";
          return result;
        } catch (error) {
          if (error instanceof ProtocolError) throw error;
          const ivy = IvyError.from(error);
          code = ivy.code;
          outcome = ivy.outcome;
          return failure(ivy, options.resourceMetadataUrl);
        } finally {
          options.observeCall?.({
            requestId,
            internalRequestIds: internalRequestIds.slice(0, 2),
            tool: call.params.name.slice(0, 128),
            code,
            outcome,
            latencyMs: Math.max(
              0,
              Math.min(86_400_000, Date.now() - startedAt),
            ),
            ...(operationIdentity ? { operationIdentity } : {}),
          });
        }
      });
      return server;
    },
    { legacy: "stateless", responseMode: "json" },
  );
  try {
    await toNodeHandler(handler)(
      request as unknown as NodeIncomingMessageLike,
      response,
      body,
    );
  } finally {
    await handler.close();
  }
}
