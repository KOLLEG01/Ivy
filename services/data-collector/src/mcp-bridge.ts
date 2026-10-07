import { randomBytes, timingSafeEqual } from "node:crypto";
import { createServer } from "node:http";
import { IvyError } from "../../../packages/sdk/src/node.js";
import type { Wire } from "../../../packages/sdk/src/node.js";

export interface TaskMcp {
  call(tool: string, args: Wire.Json): Promise<Wire.Json>;
  close(): Promise<void>;
}

/** Run-local transport; the worker receives no Hive or plugin credentials. */
export async function openMcpBridge(session: TaskMcp, signal: AbortSignal) {
  const token = randomBytes(32).toString("base64url");
  const server = createServer(async (request, response) => {
    const supplied =
      request.headers.authorization?.replace(/^Bearer /, "") ?? "";
    if (
      request.method !== "POST" ||
      request.url !== "/call" ||
      Buffer.byteLength(supplied) !== Buffer.byteLength(token) ||
      !timingSafeEqual(Buffer.from(supplied), Buffer.from(token))
    ) {
      response.writeHead(401).end();
      request.resume();
      return;
    }
    try {
      signal.throwIfAborted();
      const chunks: Buffer[] = [];
      let bytes = 0;
      for await (const chunk of request) {
        bytes += chunk.length;
        if (bytes > 1048576)
          throw new IvyError(
            "configuration_invalid",
            "MCP arguments exceed the task budget.",
          );
        chunks.push(chunk);
      }
      const value = JSON.parse(Buffer.concat(chunks).toString("utf8")) as {
        tool?: unknown;
        arguments?: Wire.Json;
      };
      if (typeof value.tool !== "string")
        throw new IvyError(
          "configuration_invalid",
          "MCP tool name is required.",
        );
      const result = await session.call(value.tool, value.arguments ?? {});
      const encoded = JSON.stringify({ result });
      if (Buffer.byteLength(encoded) > 1048576)
        throw new IvyError(
          "provider_unavailable",
          "MCP result exceeds the task budget.",
        );
      response
        .writeHead(200, { "Content-Type": "application/json" })
        .end(encoded);
    } catch (error) {
      const code =
        IvyError.from(error).code === "configuration_invalid"
          ? "configuration_invalid"
          : "provider_unavailable";
      response
        .writeHead(400, { "Content-Type": "application/json" })
        .end(JSON.stringify({ error: { code } }));
    }
  });
  server.requestTimeout = 15000;
  server.headersTimeout = 10000;
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  if (!address || typeof address === "string")
    throw new IvyError(
      "provider_unavailable",
      "The task MCP bridge is unavailable.",
    );
  return {
    context: { url: `http://127.0.0.1:${address.port}/call`, token },
    async close() {
      server.closeAllConnections();
      await new Promise<void>((resolve, reject) =>
        server.close((error) => (error ? reject(error) : resolve())),
      );
      await session.close();
    },
  };
}
