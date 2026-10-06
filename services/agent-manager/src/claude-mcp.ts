import { readFile, lstat } from "node:fs/promises";
import { isAbsolute } from "node:path";
import { atomicJson } from "../../../packages/host-runtime/src/config.js";
import {
  digest,
  hashJson,
  IvyError,
  requireThat,
} from "../../../packages/sdk/src/node.js";
import type { Agent } from "../../../packages/sdk/src/node.js";
import type { ServiceConnection } from "../../../packages/sdk/src/service.js";
import {
  readEnvironmentDocument,
  selectEnvironmentDocument,
} from "./environment.js";

/** Project the existing Hive MCP document into the pinned Claude SDK adapter. */
export class ClaudeMcpConfigurationManager {
  status: Agent.ManagedOutputStatus;
  private running: Promise<void> | null = null;
  private lastHash: string | null = null;
  constructor(
    readonly target: string,
    readonly hostId: string,
    readonly hiveUrl: string,
    readonly credential: string,
    readonly defaults: Agent.McpDocument,
  ) {
    requireThat(
      isAbsolute(target),
      "invalid_arguments",
      "Claude MCP configuration path must be absolute.",
    );
    this.status = {
      state: "pending",
      target,
      desiredVersion: null,
      appliedVersion: null,
      observedAt: new Date().toISOString(),
      code: null,
      bytes: 0,
    };
  }
  synchronize(connection: Pick<ServiceConnection, "request">): Promise<void> {
    if (!this.running)
      this.running = this.apply(connection)
        .catch((error) => {
          const code = IvyError.from(error).code;
          this.status = {
            ...this.status,
            state: [
              "target_conflict",
              "invalid_arguments",
              "access_denied",
            ].includes(code)
              ? "conflict"
              : "pending",
            observedAt: new Date().toISOString(),
            code,
          };
        })
        .finally(() => {
          this.running = null;
        });
    return this.running;
  }
  bootstrap(): Promise<void> {
    return this.synchronize({
      request: async () => {
        throw new IvyError("not_found", "No stored environment override.");
      },
    } as unknown as Pick<ServiceConnection, "request">);
  }
  async close(): Promise<void> {
    await this.running;
  }
  private async apply(
    connection: Pick<ServiceConnection, "request">,
  ): Promise<void> {
    const global = await readEnvironmentDocument<Agent.McpDocument>(
      connection,
      "mcp",
      null,
      "McpDocument",
    );
    const host = await readEnvironmentDocument<Agent.McpDocument>(
      connection,
      "mcp",
      this.hostId,
      "McpDocument",
    );
    const desired = selectEnvironmentDocument(this.defaults, global, host);
    const allowed = new Set(
      ["/mcp", "/mcp-dev"].map(
        (path) => new URL(this.hiveUrl.replace(/\/$/, "") + path).href,
      ),
    );
    const servers: Record<string, unknown> = {};
    if (desired.document.enabled)
      for (const server of desired.document.servers) {
        if (!server.enabled) continue;
        const url = new URL(server.url);
        const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(
          url.hostname,
        );
        requireThat(
          (url.protocol === "https:" ||
            (url.protocol === "http:" && loopback)) &&
            !url.username &&
            !url.password &&
            !url.hash,
          "invalid_arguments",
          "Managed remote MCP URLs require HTTPS.",
        );
        if (server.authentication === "agent-manager")
          requireThat(
            allowed.has(url.href),
            "access_denied",
            "AgentManager credentials may only reach this Hive MCP resource.",
          );
        requireThat(
          !Object.hasOwn(servers, server.name),
          "invalid_arguments",
          "Managed MCP server names must be unique.",
        );
        servers[server.name] = {
          type: "http",
          url: url.href,
          ...(server.authentication === "agent-manager"
            ? { headers: { Authorization: "Bearer " + this.credential } }
            : {}),
        };
      }
    let current: Buffer | null = null;
    const statePath = this.target + ".state.json";
    const authority = hashJson({ hostId: this.hostId, hiveUrl: this.hiveUrl });
    let state: {
      schemaVersion: 1;
      authority: string;
      appliedHash: string;
    } | null = null;
    try {
      const info = await lstat(this.target);
      requireThat(
        info.isFile() && !info.isSymbolicLink() && info.size <= 4 * 1024 * 1024,
        "target_conflict",
        "Claude MCP configuration was replaced by another object.",
      );
      current = await readFile(this.target);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    }
    try {
      const info = await lstat(statePath);
      requireThat(
        info.isFile() && !info.isSymbolicLink() && info.size <= 4096,
        "target_conflict",
        "Claude MCP ownership state was replaced.",
      );
      state = JSON.parse(await readFile(statePath, "utf8")) as {
        schemaVersion: 1;
        authority: string;
        appliedHash: string;
      };
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    }
    requireThat(
      (current === null && state === null) ||
        (current !== null &&
          state !== null &&
          state.schemaVersion === 1 &&
          state.authority === authority &&
          state.appliedHash === digest(current)),
      "target_conflict",
      "Claude MCP configuration changed outside this AgentManager.",
    );
    if (this.lastHash !== null)
      requireThat(
        current !== null && digest(current) === this.lastHash,
        "target_conflict",
        "Claude MCP configuration changed outside AgentManager.",
      );
    const serialized = JSON.stringify(servers);
    requireThat(
      Buffer.byteLength(serialized) <= 4 * 1024 * 1024,
      "limit_exceeded",
      "Claude MCP configuration is too large.",
    );
    await atomicJson(this.target, servers);
    const saved = await readFile(this.target);
    this.lastHash = digest(saved);
    await atomicJson(statePath, {
      schemaVersion: 1,
      authority,
      appliedHash: this.lastHash,
    });
    const version = hashJson({
      selection: desired.version,
      servers: Object.keys(servers),
    });
    this.status = {
      state: desired.document.enabled ? "applied" : "disabled",
      target: this.target,
      desiredVersion: version,
      appliedVersion: version,
      observedAt: new Date().toISOString(),
      code: null,
      bytes: saved.length,
    };
  }
}
