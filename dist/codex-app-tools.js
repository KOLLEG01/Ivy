import {
  fileHash
} from "./chunk-HOQXZO42.js";
import {
  SchemaValidators
} from "./chunk-24P52CL7.js";
import "./chunk-BVUWPE6Y.js";
import {
  IvyError,
  requireThat
} from "./chunk-BO4WKKA7.js";

// packages/sdk/src/codex-app-tools.ts
import { randomUUID } from "node:crypto";
import { readdirSync } from "node:fs";
import { isAbsolute, dirname } from "node:path";
import { Client } from "@modelcontextprotocol/client";
import { StdioClientTransport } from "@modelcontextprotocol/client/stdio";

// specs/schemas/codex-app-tools.schema.json
var codex_app_tools_schema_default = {
  $schema: "https://json-schema.org/draft/2020-12/schema",
  $defs: {
    AppThreadObservation: {
      type: "object",
      additionalProperties: false,
      required: ["id", "hostId", "kind", "status"],
      properties: {
        id: { $ref: "#/$defs/ThreadId" },
        hostId: { const: "local" },
        kind: { const: "codex" },
        status: {
          type: "object",
          additionalProperties: false,
          required: ["type", "activeFlags"],
          properties: {
            type: { type: "string", minLength: 1, maxLength: 64 },
            activeFlags: { type: "array", maxItems: 64, uniqueItems: true, items: { type: "string", minLength: 1, maxLength: 64 } }
          }
        }
      }
    },
    AppArchiveAcknowledgement: {
      type: "object",
      additionalProperties: false,
      required: ["threadId", "archived"],
      properties: { threadId: { $ref: "#/$defs/ThreadId" }, archived: { const: true } }
    },
    AppPromptAcknowledgement: {
      type: "object",
      additionalProperties: false,
      required: ["threadId", "sent"],
      properties: { threadId: { $ref: "#/$defs/ThreadId" }, sent: { const: true } }
    },
    AppThreadList: {
      type: "array",
      maxItems: 256,
      items: {
        type: "object",
        additionalProperties: false,
        required: ["id", "hostId", "kind", "status"],
        properties: {
          id: { $ref: "#/$defs/ThreadId" },
          hostId: { const: "local" },
          kind: { const: "codex" },
          status: { type: "string", minLength: 1, maxLength: 64 }
        }
      }
    },
    ThreadId: { type: "string", pattern: "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$" },
    AppToolsSettings: {
      type: "object",
      additionalProperties: false,
      required: ["nodeExecutable", "nodeExecutableHash", "serverPath", "serverHash", "pipePath", "actorThreadId"],
      properties: {
        nodeExecutable: { $ref: "#/$defs/Path" },
        nodeExecutableHash: { $ref: "#/$defs/Hash" },
        serverPath: { $ref: "#/$defs/Path" },
        serverHash: { $ref: "#/$defs/Hash" },
        pipePath: { type: "string", minLength: 1, maxLength: 1024 },
        actorThreadId: { type: "string", pattern: "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$" },
        inputSchemasHash: { $ref: "#/$defs/Hash", deprecated: true, description: "Ignored legacy setup snapshot; accepted for existing instance configurations." }
      }
    },
    Path: { type: "string", minLength: 1, maxLength: 32767 },
    Hash: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" }
  }
};

// packages/sdk/src/codex-app-tools.ts
var runtimeNames = [
  "list_threads",
  "navigate_to_codex_page",
  "read_thread",
  "send_message_to_thread",
  "set_thread_archived"
];
var schemas = new SchemaValidators();
var uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
function appThreadObservation(result, threadId) {
  const value = result;
  const thread = value?.thread;
  const flags = thread?.status?.activeFlags ?? (["idle", "notLoaded"].includes(thread?.status?.type ?? "") ? [] : void 0);
  requireThat(
    value?.schemaVersion === 1 && thread?.id === threadId && thread.hostId === "local" && thread.kind === "codex" && typeof thread.status?.type === "string" && Array.isArray(flags) && flags.every((flag) => typeof flag === "string") && thread.status.activeFlags !== null,
    "app_tools_identity_changed",
    "App observation does not match the original local Codex task."
  );
  const observed = {
    id: thread.id,
    hostId: thread.hostId,
    kind: thread.kind,
    status: { type: thread.status.type, activeFlags: [...flags] }
  };
  schemas.validate(
    { $defs: codex_app_tools_schema_default.$defs, $ref: "#/$defs/AppThreadObservation" },
    observed,
    16384
  );
  return observed;
}
var CodexAppTools = class _CodexAppTools {
  client = null;
  settings;
  inputSchemas = /* @__PURE__ */ new Map();
  connecting = null;
  closed = false;
  inspectionOnly = false;
  constructor(settings) {
    schemas.validate(
      { $defs: codex_app_tools_schema_default.$defs, $ref: "#/$defs/AppToolsSettings" },
      settings,
      131072
    );
    requireThat(
      isAbsolute(settings.nodeExecutable) && isAbsolute(settings.serverPath) && (settings.pipePath === "auto" || settings.pipePath.startsWith("\\\\.\\pipe\\")),
      "invalid_arguments",
      "App Tools requires exact installed paths and an explicit or automatically discovered local Desktop pipe."
    );
    this.settings = structuredClone(settings);
  }
  /** Explicit read-only installation setup verifies the current App Tools actor. */
  static async inspect(settings) {
    const client = new _CodexAppTools(settings);
    client.inspectionOnly = true;
    try {
      await client.verifyActor();
      return structuredClone(settings);
    } finally {
      await client.close();
    }
  }
  async verifyFiles() {
    const hashes = await Promise.all([
      fileHash(this.settings.nodeExecutable),
      fileHash(this.settings.serverPath)
    ]);
    requireThat(
      hashes[0] === this.settings.nodeExecutableHash && hashes[1] === this.settings.serverHash,
      "app_tools_build_changed",
      "App Tools executable or bundled server changed."
    );
  }
  connect() {
    requireThat(
      !this.closed,
      "app_tools_closed",
      "Original App Tools connection is closed."
    );
    return this.connecting ??= (async () => {
      await this.verifyFiles();
      requireThat(
        !this.closed,
        "app_tools_closed",
        "App Tools was closed before startup."
      );
      let last = new IvyError(
        "app_tools_pipe_unavailable",
        "No current Codex Desktop App Tools pipe is available."
      );
      for (const pipePath of this.pipePaths()) {
        const client = new Client({ name: "ivy-app-tools", version: "1.0.0" });
        const transport = new StdioClientTransport({
          command: this.settings.nodeExecutable,
          args: [this.settings.serverPath],
          cwd: dirname(this.settings.serverPath),
          stderr: "ignore",
          maxBufferSize: 1024 * 1024,
          env: { CODEX_APP_TOOLS_PIPE_PATH: pipePath }
        });
        try {
          await client.connect(transport, { timeout: 1e4 });
          await this.verifyFiles();
          const catalog = await client.listTools({}, { timeout: 1e4 });
          requireThat(
            catalog.nextCursor === void 0,
            "app_tools_contract_changed",
            "A complete App Tools catalogue is required."
          );
          const selected = /* @__PURE__ */ new Map();
          for (const name of runtimeNames) {
            const matches = catalog.tools.filter((tool) => tool.name === name);
            requireThat(
              matches.length === 1,
              "app_tools_contract_changed",
              "Required App Tools method is missing or duplicated."
            );
            selected.set(name, matches[0].inputSchema);
          }
          this.client = client;
          this.inputSchemas.clear();
          for (const [name, schema] of selected)
            this.inputSchemas.set(name, schema);
          client.onclose = () => {
            if (this.client === client) {
              this.client = null;
              this.closed = true;
            }
          };
          return;
        } catch (error) {
          last = error;
          if (this.client === client) this.client = null;
          try {
            await client.close();
          } catch {
          }
        }
      }
      this.closed = true;
      throw last;
    })();
  }
  pipePaths() {
    if (this.settings.pipePath !== "auto") return [this.settings.pipePath];
    const prefix = "codex-browser-use-", current = process.env["CODEX_APP_TOOLS_PIPE_PATH"];
    const names = /* @__PURE__ */ new Set();
    if (current?.startsWith("\\\\.\\pipe\\" + prefix)) names.add(current);
    try {
      for (const name of readdirSync("\\\\.\\pipe\\"))
        if (name.startsWith(prefix)) names.add("\\\\.\\pipe\\" + name);
    } catch {
    }
    return [...names];
  }
  async invoke(name, args, operationId) {
    requireThat(
      uuid.test(operationId),
      "invalid_arguments",
      "Original App Tools operation ID required."
    );
    await this.connect();
    schemas.validate(this.inputSchemas.get(name), args, 65536);
    const client = this.client;
    requireThat(
      client,
      "app_tools_closed",
      "App Tools connection is unavailable."
    );
    const result = await client.callTool(
      {
        name,
        arguments: args,
        _meta: { thread_id: this.settings.actorThreadId, call_id: operationId }
      },
      { timeout: 3e4 }
    );
    const content = result.content;
    requireThat(
      result.isError !== true && Array.isArray(content) && content.length === 1 && content[0]?.type === "text" && typeof content[0]["text"] === "string" && Buffer.byteLength(content[0]["text"]) <= 1024 * 1024,
      "app_tools_response_invalid",
      "App Tools did not return a bounded successful JSON result."
    );
    return JSON.parse(content[0]["text"]);
  }
  async readThread(threadId) {
    requireThat(
      uuid.test(threadId),
      "invalid_arguments",
      "Exact App thread ID required."
    );
    return appThreadObservation(
      await this.invoke(
        "read_thread",
        {
          threadId,
          hostId: "local",
          turnLimit: 1,
          includeOutputs: false,
          maxOutputCharsPerItem: 0
        },
        randomUUID()
      ),
      threadId
    );
  }
  verifyActor() {
    return this.readThread(this.settings.actorThreadId);
  }
  async navigateToThread(threadId) {
    requireThat(
      uuid.test(threadId),
      "invalid_arguments",
      "Exact App thread ID required."
    );
    const result = await this.invoke(
      "navigate_to_codex_page",
      { threadId },
      randomUUID()
    );
    requireThat(
      result?.navigated === true,
      "app_tools_response_invalid",
      "App Tools did not confirm the requested local task was loaded."
    );
  }
  async listThreads(limit = 50) {
    requireThat(
      Number.isInteger(limit) && limit >= 1 && limit <= 50,
      "invalid_arguments",
      "Bounded App thread limit required."
    );
    const value = await this.invoke(
      "list_threads",
      { limit },
      randomUUID()
    );
    requireThat(
      value?.schemaVersion === 4 && Array.isArray(value.pinnedThreads) && Array.isArray(value.threads),
      "app_tools_response_invalid",
      "App thread catalogue is invalid."
    );
    const threads = [...value.pinnedThreads, ...value.threads].filter((thread) => {
      const item = thread;
      return item?.kind === "codex" && item.hostId === "local" && typeof item.id === "string" && uuid.test(item.id) && typeof item.status === "string";
    }).map((thread) => ({
      id: thread.id,
      hostId: thread.hostId,
      kind: thread.kind,
      status: thread.status
    }));
    schemas.validate(
      { $defs: codex_app_tools_schema_default.$defs, $ref: "#/$defs/AppThreadList" },
      threads,
      65536
    );
    return threads;
  }
  /** A successful MCP result is the App-owned acknowledgement. The caller records intent
   * before submission and must treat every later failure as unknown. */
  async sendMessage(threadId, prompt, operationId, beforeSubmit) {
    requireThat(
      !this.inspectionOnly,
      "app_tools_read_only",
      "Installation inspection cannot send task messages."
    );
    requireThat(
      uuid.test(threadId) && uuid.test(operationId) && threadId !== this.settings.actorThreadId && prompt.length > 0 && prompt.length <= 4096,
      "invalid_arguments",
      "Exact prompt operation and target task required."
    );
    await this.connect();
    await this.verifyActor();
    const args = { threadId, hostId: "local", prompt };
    schemas.validate(
      this.inputSchemas.get("send_message_to_thread"),
      args,
      65536
    );
    await beforeSubmit();
    try {
      await this.invoke("send_message_to_thread", args, operationId);
      return { threadId, sent: true };
    } catch {
      throw new IvyError(
        "app_tools_prompt_unknown",
        "Original Desktop prompt result is unknown; do not repeat it.",
        "unknown"
      );
    }
  }
  /** Caller durably records intent immediately before this callback permits submission.
   * All exceptions after the callback are unknown; no retry or replacement operation. */
  async archiveThread(threadId, operationId, beforeSubmit) {
    requireThat(
      !this.inspectionOnly,
      "app_tools_read_only",
      "Installation inspection cannot archive tasks."
    );
    requireThat(
      uuid.test(threadId) && uuid.test(operationId) && threadId !== this.settings.actorThreadId,
      "invalid_arguments",
      "Exact archive operation/target required; target cannot be the assigned controller task."
    );
    await this.connect();
    await this.verifyActor();
    schemas.validate(
      this.inputSchemas.get("set_thread_archived"),
      { threadId, hostId: "local", archived: true },
      65536
    );
    await beforeSubmit();
    try {
      const result = await this.invoke(
        "set_thread_archived",
        { threadId, hostId: "local", archived: true },
        operationId
      );
      requireThat(
        result?.threadId === threadId && result.archived === true,
        "app_tools_response_invalid",
        "Archive acknowledgement changed target or outcome."
      );
      return { threadId, archived: true };
    } catch {
      throw new IvyError(
        "app_tools_archive_unknown",
        "Original Desktop archive result is unknown; do not repeat it.",
        "unknown"
      );
    }
  }
  async close() {
    this.closed = true;
    const client = this.client;
    this.client = null;
    if (client) await client.close();
  }
};
export {
  CodexAppTools,
  appThreadObservation
};
//# sourceMappingURL=codex-app-tools.js.map
