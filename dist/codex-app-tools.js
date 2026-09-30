import {
  accountHome
} from "./chunk-OFJA23D2.js";
import {
  SchemaValidators
} from "./chunk-Y22UBH3V.js";
import "./chunk-7BQWA3HX.js";
import {
  IvyError,
  requireThat
} from "./chunk-T2KPXKB3.js";

// packages/sdk/src/codex-app-tools.ts
import { randomUUID } from "node:crypto";
import { readdirSync, statSync } from "node:fs";
import { isAbsolute, dirname, join, resolve, sep } from "node:path";
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
      required: ["pipePath", "actorThreadId"],
      properties: {
        nodeExecutable: { $ref: "#/$defs/Path", deprecated: true, description: "Optional explicit test/runtime override. Ignored for bundled App Tools." },
        nodeExecutableHash: { $ref: "#/$defs/Hash", deprecated: true, description: "Ignored legacy setup pin." },
        serverPath: { $ref: "#/$defs/Path", deprecated: true, description: "Optional explicit test/runtime override. Bundled App Tools always use the newest installed version." },
        serverHash: { $ref: "#/$defs/Hash", deprecated: true, description: "Ignored legacy setup pin." },
        pipePath: { type: "string", minLength: 1, maxLength: 1024 },
        actorThreadId: { type: "string", pattern: "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$" },
        inputSchemasHash: { $ref: "#/$defs/Hash", deprecated: true, description: "Ignored legacy setup snapshot; accepted for existing instance configurations." },
        internalProjectRoot: { $ref: "#/$defs/Path" }
      }
    },
    Path: { type: "string", minLength: 1, maxLength: 32767 },
    Hash: { type: "string", pattern: "^sha256:[0-9a-f]{64}$" }
  }
};

// packages/sdk/src/codex-app-tools.ts
var runtimeNames = [
  "list_projects",
  "list_threads",
  "navigate_to_codex_page",
  "read_thread",
  "create_thread",
  "transfer_voice_call",
  "send_message_to_thread",
  "set_thread_archived"
];
var schemas = new SchemaValidators();
var uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
var bundledRoot = () => join(accountHome(), ".codex", "plugins", "cache", "openai-bundled", "codex-app-tools");
var version = /^\d+\.\d+\.\d+$/;
function latestBundledAppToolsServer(root = bundledRoot()) {
  let names;
  try {
    names = readdirSync(root, { withFileTypes: true }).filter((entry) => entry.isDirectory() && version.test(entry.name)).map((entry) => entry.name);
  } catch {
    names = [];
  }
  names.sort((a, b) => b.localeCompare(a, "en", { numeric: true }));
  for (const name of names) {
    const path = join(root, name, "server.mjs");
    try {
      if (statSync(path).isFile()) return path;
    } catch {
    }
  }
  throw new IvyError("app_tools_unavailable", "No installed bundled Codex App Tools server is available.");
}
function bundledServer(path, bundleRoot = bundledRoot()) {
  if (!path) return true;
  const root = resolve(bundleRoot) + sep;
  const selected = resolve(path);
  return process.platform === "win32" ? selected.toLowerCase().startsWith(root.toLowerCase()) : selected.startsWith(root);
}
function resolveAppToolsLaunch(settings, bundleRoot = bundledRoot()) {
  const automatic = bundledServer(settings.serverPath, bundleRoot);
  return {
    nodeExecutable: automatic ? process.execPath : settings.nodeExecutable ?? process.execPath,
    serverPath: automatic ? latestBundledAppToolsServer(bundleRoot) : settings.serverPath
  };
}
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
  actorVerified = false;
  settings;
  inputSchemas = /* @__PURE__ */ new Map();
  connecting = null;
  closed = false;
  inspectionOnly = false;
  connectedServer = null;
  get isClosed() {
    return this.closed;
  }
  get isCurrentInstallation() {
    if (!this.connectedServer) return true;
    try {
      return this.connectedServer === resolveAppToolsLaunch(this.settings).serverPath;
    } catch {
      return false;
    }
  }
  constructor(settings) {
    schemas.validate(
      { $defs: codex_app_tools_schema_default.$defs, $ref: "#/$defs/AppToolsSettings" },
      settings,
      131072
    );
    requireThat(
      (!settings.nodeExecutable || isAbsolute(settings.nodeExecutable)) && (!settings.serverPath || isAbsolute(settings.serverPath)) && (settings.pipePath === "auto" || settings.pipePath.startsWith("\\\\.\\pipe\\")),
      "invalid_arguments",
      "App Tools requires local installed paths and an explicit or automatically discovered Desktop pipe."
    );
    this.settings = structuredClone(settings);
  }
  /** Explicit read-only installation setup verifies the current App Tools actor. */
  static async inspect(settings) {
    const client = new _CodexAppTools(settings);
    client.inspectionOnly = true;
    try {
      await client.verifyActor();
      await client.internalProjectId();
      return structuredClone(settings);
    } finally {
      await client.close();
    }
  }
  connect() {
    requireThat(
      !this.closed,
      "app_tools_closed",
      "Original App Tools connection is closed."
    );
    return this.connecting ??= (async () => {
      const { serverPath, nodeExecutable } = resolveAppToolsLaunch(this.settings);
      requireThat(
        !this.closed,
        "app_tools_closed",
        "App Tools was closed before startup."
      );
      let last = new IvyError(
        "app_tools_pipe_unavailable",
        "No current Codex Desktop App Tools pipe is available."
      );
      const paths = this.pipePaths();
      let next = 0, connected = false;
      const findPipe = async () => {
        while (!connected && next < paths.length) {
          const pipePath = paths[next++];
          const client = new Client({ name: "ivy-app-tools", version: "1.0.0" });
          const transport = new StdioClientTransport({
            command: nodeExecutable,
            args: [serverPath],
            cwd: dirname(serverPath),
            stderr: "ignore",
            maxBufferSize: 1024 * 1024,
            env: { CODEX_APP_TOOLS_PIPE_PATH: pipePath }
          });
          try {
            await client.connect(transport, { timeout: 3e4 });
            const catalog = await client.listTools({}, { timeout: 3e4 });
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
            if (connected) {
              await client.close();
              return;
            }
            requireThat(!this.closed, "app_tools_closed", "App Tools closed during discovery.");
            connected = true;
            this.client = client;
            this.connectedServer = serverPath;
            this.inputSchemas.clear();
            for (const [name, schema] of selected)
              this.inputSchemas.set(name, schema);
            client.onclose = () => {
              if (this.client === client) {
                this.client = null;
                this.connectedServer = null;
                this.closed = true;
              }
            };
            return;
          } catch (error) {
            last = error;
            try {
              await client.close();
            } catch {
            }
          }
        }
        throw last;
      };
      try {
        await Promise.any(Array.from({ length: Math.min(4, paths.length) }, findPipe));
      } catch {
        this.closed = true;
        throw last;
      }
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
  async invoke(name, args, operationId, actorThreadId = this.settings.actorThreadId) {
    requireThat(
      uuid.test(operationId) && uuid.test(actorThreadId),
      "invalid_arguments",
      "Original App Tools operation and actor IDs required."
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
        _meta: { thread_id: actorThreadId, call_id: operationId }
      },
      { timeout: 3e4 }
    );
    const content = result.content;
    if (result.isError === true && name === "transfer_voice_call" && Array.isArray(content) && content.length === 1 && content[0]?.type === "text" && typeof content[0]["text"] === "string" && /^(?:Error:\s*)?The current voice call could not be transferred to that task\.$/.test(
      content[0]["text"].trim()
    ))
      throw new IvyError("app_tools_voice_not_ready", "Desktop Voice is not ready to transfer yet.");
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
  async verifyActor() {
    const actor = await this.readThread(this.settings.actorThreadId);
    this.actorVerified = true;
    return actor;
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
  /** Move the Voice session that Desktop just started into the prepared local task.
   * New Desktop builds may resume their most recent Voice task despite navigation. */
  async transferVoiceCall(sourceThreadId, threadId) {
    requireThat(
      !this.inspectionOnly && uuid.test(sourceThreadId) && uuid.test(threadId) && sourceThreadId !== threadId,
      "invalid_arguments",
      "Exact source and destination Voice task IDs required."
    );
    requireThat(
      this.actorVerified,
      "app_tools_actor_unverified",
      "The assigned App Tools controller must be verified before Voice transfer."
    );
    try {
      const result = await this.invoke("transfer_voice_call", { threadId }, randomUUID(), sourceThreadId);
      requireThat(
        result?.transferred === true && (result.threadId === void 0 || result.threadId === threadId),
        "app_tools_response_invalid",
        "Desktop did not confirm Voice transfer to the exact prepared task."
      );
    } catch (error) {
      if (IvyError.from(error).code === "app_tools_voice_not_ready") throw error;
      throw new IvyError(
        "app_tools_transfer_unknown",
        "Desktop did not confirm the Voice transfer; observe its task before continuing.",
        "unknown",
        { causeCode: IvyError.from(error).code }
      );
    }
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
  async internalProjectId() {
    const root = resolve(this.settings.internalProjectRoot ?? join(accountHome(), ".ivy", "codex-projects"));
    const value = await this.invoke("list_projects", {}, randomUUID());
    requireThat(
      value?.schemaVersion === 2 && Array.isArray(value.projects),
      "app_tools_response_invalid",
      "Desktop project catalogue is invalid."
    );
    const samePath = (path) => process.platform === "win32" ? resolve(path).toLowerCase() === root.toLowerCase() : resolve(path) === root;
    const project = value.projects.find((entry) => {
      const item = entry;
      return item?.projectKind === "local" && item.hostId === "local" && typeof item.path === "string" && samePath(item.path) && typeof item.projectId === "string" && uuid.test(item.projectId);
    });
    requireThat(
      project,
      "app_tools_internal_project_missing",
      `Register ${root} as a local Codex Desktop project before PhoneBridge creates a Voice task.`
    );
    return project.projectId;
  }
  /** The App creates a user-owned local task. A lost acknowledgement cannot authorize
   * another creation; the caller journals the original attempt before submission. */
  async createLocalThread(prompt, selection, operationId, beforeSubmit) {
    requireThat(
      !this.inspectionOnly && prompt.length > 0 && prompt.length <= 4096,
      "invalid_arguments",
      "A bounded task creation prompt is required."
    );
    await this.connect();
    await this.verifyActor();
    const projectId = await this.internalProjectId();
    const args = {
      prompt,
      target: { type: "project", projectId, environment: { type: "local" } },
      title: "PhoneBridge Voice",
      model: selection.model,
      thinking: selection.reasoningEffort
    };
    schemas.validate(this.inputSchemas.get("create_thread"), args, 65536);
    await beforeSubmit();
    let value;
    try {
      value = await this.invoke("create_thread", args, operationId);
    } catch {
      throw new IvyError("app_tools_create_unknown", "Original Desktop task creation result is unknown; do not create another task.", "unknown");
    }
    const result = value;
    if (!(result && typeof result.threadId === "string" && uuid.test(result.threadId) && result.hostId === "local"))
      throw new IvyError("app_tools_create_unknown", "Desktop did not confirm one exact local task; do not create another task.", "unknown");
    return result.threadId;
  }
  /** A successful MCP result is the App-owned acknowledgement. The caller records intent
   * before submission and must treat every later failure as unknown. */
  async sendMessage(threadId, prompt, operationId, beforeSubmit, selection = {}) {
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
    if (!this.actorVerified) await this.verifyActor();
    const args = {
      threadId,
      hostId: "local",
      prompt,
      ...selection.model ? { model: selection.model } : {},
      ...selection.reasoningEffort ? { thinking: selection.reasoningEffort } : {}
    };
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
    this.connectedServer = null;
    if (client) await client.close();
  }
};
export {
  CodexAppTools,
  appThreadObservation,
  latestBundledAppToolsServer,
  resolveAppToolsLaunch
};
//# sourceMappingURL=codex-app-tools.js.map
