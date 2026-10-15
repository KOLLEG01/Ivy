import { isAbsolute } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import {
  IvyError,
  NativeOwner,
  newOperationId,
  responseValue,
  serviceTools,
  validateAgent,
} from "../../../packages/sdk/src/node.js";
import type { Agent, RpcClient, Wire } from "../../../packages/sdk/src/node.js";
import type { Task } from "./schema.js";

export function mcpToolResult(value: Record<string, Wire.Json>): Wire.Json {
  let payload = value["structuredContent"];
  let parsed = payload !== undefined && payload !== null;
  if (payload === undefined || payload === null) {
    const content = value["content"];
    if (Array.isArray(content))
      for (const entry of content) {
        if (
          entry && typeof entry === "object" && !Array.isArray(entry) &&
          entry["type"] === "text" && typeof entry["text"] === "string"
        ) {
          try {
            payload = JSON.parse(entry["text"]) as Wire.Json;
            parsed = true;
            break;
          } catch { /* Preserve non-JSON MCP content below. */ }
        }
      }
  }
  if (value["isError"] === true) {
    if (
      payload && typeof payload === "object" &&
      !Array.isArray(payload) && "error" in payload
    ) {
      try {
        responseValue({ jsonrpc: "2.0", id: "mcp-tool", error: payload["error"] }, "mcp-tool");
      } catch (error) {
        if (
          error instanceof IvyError && error.code !== "invalid_frame" &&
          /^[a-z][a-z0-9_]{0,127}$/.test(error.code)
        )
          throw new IvyError(error.code, "The MCP tool returned an error.", error.outcome);
      }
    }
    throw new IvyError("provider_unavailable", "The MCP tool returned an error.", "unknown");
  }
  return parsed ? payload! : value;
}

/** A technical, ephemeral MCP context. This transport never starts a model turn. */
export class NativeMcpSession {
  private owner: NativeOwner | null = null;
  private epoch: string | null = null;
  private threadId: string | null = null;
  private tail: Promise<unknown> = Promise.resolve();
  constructor(
    readonly client: RpcClient,
    readonly configuration: NonNullable<Task["mcp"]>,
    readonly signal: AbortSignal,
  ) {
    if (!isAbsolute(configuration.cwd))
      throw new IvyError(
        "configuration_invalid",
        "MCP working directory must be absolute.",
      );
  }
  private async invoke(
    method: string,
    params: Wire.Json,
    signal = this.signal,
  ): Promise<Wire.Json> {
    signal.throwIfAborted();
    const owner = this.owner!;
    const active =
      signal === this.signal
        ? owner
        : new NativeOwner(this.client, owner.caller, owner.target, { signal });
    const binding = await active.binding(method);
    const call = {
      operationId: await newOperationId(this.client),
      method,
      params,
      definitionHash: binding.definitionHash,
    };
    let operation = await active.dispatch(call, async () =>
      signal.throwIfAborted(),
    );
    while (!operation || !["succeeded", "failed"].includes(operation.phase)) {
      await delay(100, undefined, { signal });
      const observed = await active.operation(call);
      if ("phase" in observed) operation = observed;
    }
    active.checkOperation(operation, call);
    if (
      operation.epoch !== this.epoch ||
      operation.phase !== "succeeded" ||
      !operation.reply ||
      !("result" in operation.reply)
    )
      throw new IvyError(
        "provider_unavailable",
        "The original MCP operation did not complete on the selected owner.",
      );
    return operation.reply.result;
  }
  private async initialize(): Promise<void> {
    if (this.threadId) return;
    const status = await serviceTools(
      this.client,
      this.configuration.serviceNodeId,
      [{ namespace: "agent", interfaceVersion: "1.0.0" }],
    ).call("agent.status", {});
    validateAgent("Status", status);
    const target = status as Agent.Status;
    if (target.state !== "ready" || !target.epoch)
      throw new IvyError(
        "provider_unavailable",
        "The MCP native owner is not ready.",
      );
    const identity = await this.client.request("system.status", {});
    this.owner = new NativeOwner(
      this.client,
      identity.callerPrincipalId,
      {
        serviceNodeId: target.serviceNodeId,
        hostId: target.hostId,
        nativeVersion: target.nativeVersion,
        nativeExecutableHash: target.nativeExecutableHash,
        catalogHash: target.catalogHash,
      },
      { signal: this.signal },
    );
    this.epoch = target.epoch;
    const params = await this.owner.threadStartParams({
      cwd: this.configuration.cwd,
      ephemeral: true,
      permissions: ":read-only",
      approvalPolicy: "never",
    });
    const value = (await this.invoke("thread/start", params)) as Record<
      string,
      Wire.Json
    >;
    const thread = value["thread"] as Record<string, Wire.Json> | undefined;
    if (
      !thread ||
      typeof thread["id"] !== "string" ||
      thread["ephemeral"] !== true
    )
      throw new IvyError(
        "provider_unavailable",
        "The provider did not create the requested ephemeral MCP context.",
      );
    this.threadId = thread["id"];
  }
  call(tool: string, args: Wire.Json): Promise<Wire.Json> {
    if (!this.configuration.tools.includes(tool))
      return Promise.reject(
        new IvyError(
          "configuration_invalid",
          "The task has not been granted this MCP tool.",
        ),
      );
    const work = this.tail.then(async () => {
      await this.initialize();
      const value = (await this.invoke("mcpServer/tool/call", {
        threadId: this.threadId,
        server: this.configuration.server,
        tool,
        arguments: args,
      })) as Record<string, Wire.Json>;
      return mcpToolResult(value);
    });
    this.tail = work.catch(() => undefined);
    return work;
  }
  async close(): Promise<void> {
    if (!this.threadId || !this.owner) return;
    const threadId = this.threadId;
    this.threadId = null;
    try {
      await this.invoke(
        "thread/unsubscribe",
        { threadId },
        AbortSignal.timeout(5000),
      );
    } catch {
      /* Ephemeral contexts also expire when their owner disconnects. */
    }
  }
}
