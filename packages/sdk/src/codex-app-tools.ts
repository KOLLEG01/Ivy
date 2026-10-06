import { randomUUID } from "node:crypto";
import { readdirSync, statSync } from "node:fs";
import { isAbsolute, dirname, join, resolve, sep } from "node:path";
import { Client } from "@modelcontextprotocol/client";
import { StdioClientTransport } from "@modelcontextprotocol/client/stdio";
import { IvyError, requireThat } from "../../contracts/src/errors.js";
import { SchemaValidators } from "../../contracts/src/schema.js";
import type { Schema } from "../../contracts/src/types.js";
import { accountHome } from "../../host-runtime/src/layout.js";
import contract from "../../../specs/schemas/codex-app-tools.schema.json" with { type: "json" };

export interface AppToolsSettings {
  /** Legacy explicit paths remain usable for local fixtures. Bundled paths resolve afresh. */
  nodeExecutable?: string;
  nodeExecutableHash?: string;
  serverPath?: string;
  serverHash?: string;
  pipePath: string;
  actorThreadId: string;
  internalProjectRoot?: string;
}
export interface AppThreadObservation {
  id: string;
  hostId: string;
  kind: string;
  status: { type: string; activeFlags: string[] };
}
export interface AppThreadSummary {
  id: string;
  hostId: "local";
  kind: "codex";
  status: string;
}
const runtimeNames = [
  "list_projects",
  "list_threads",
  "navigate_to_codex_page",
  "read_thread",
  "create_thread",
  "transfer_voice_call",
  "send_message_to_thread",
  "set_thread_archived",
] as const;
const schemas = new SchemaValidators();
const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const bundledRoot = () => join(accountHome(), ".codex", "plugins", "cache", "openai-bundled", "codex-app-tools");
const version = /^\d+\.\d+\.\d+$/;

/** Resolve a complete local bundled release each time a new MCP connection starts. */
export function latestBundledAppToolsServer(root = bundledRoot()): string {
  let names: string[];
  try { names = readdirSync(root, { withFileTypes: true })
    .filter(entry => entry.isDirectory() && version.test(entry.name))
    .map(entry => entry.name); }
  catch { names = []; }
  names.sort((a, b) => b.localeCompare(a, "en", { numeric: true }));
  for (const name of names) {
    const path = join(root, name, "server.mjs");
    try { if (statSync(path).isFile()) return path; } catch { /* Update may still be installing. */ }
  }
  throw new IvyError("app_tools_unavailable", "No installed bundled Codex App Tools server is available.");
}

function bundledServer(path: string | undefined, bundleRoot = bundledRoot()): boolean {
  if (!path) return true;
  const root = resolve(bundleRoot) + sep;
  const selected = resolve(path);
  return process.platform === "win32"
    ? selected.toLowerCase().startsWith(root.toLowerCase())
    : selected.startsWith(root);
}

export function resolveAppToolsLaunch(settings: AppToolsSettings, bundleRoot = bundledRoot()): {
  nodeExecutable: string; serverPath: string;
} {
  const automatic = bundledServer(settings.serverPath, bundleRoot);
  return {
    nodeExecutable: automatic ? process.execPath : settings.nodeExecutable ?? process.execPath,
    serverPath: automatic ? latestBundledAppToolsServer(bundleRoot) : settings.serverPath!,
  };
}
export function appThreadObservation(
  result: unknown,
  threadId: string,
): AppThreadObservation {
  const value = result as {
    schemaVersion?: unknown;
    thread?: AppThreadObservation;
  } | null;
  const thread = value?.thread;
  const flags =
    thread?.status?.activeFlags ??
    (["idle", "notLoaded"].includes(thread?.status?.type ?? "")
      ? []
      : undefined);
  requireThat(
    value?.schemaVersion === 1 &&
      thread?.id === threadId &&
      thread.hostId === "local" &&
      thread.kind === "codex" &&
      typeof thread.status?.type === "string" &&
      Array.isArray(flags) &&
      flags.every((flag) => typeof flag === "string") &&
      thread.status.activeFlags !== null,
    "app_tools_identity_changed",
    "App observation does not match the original local Codex task.",
  );
  const observed = {
    id: thread.id,
    hostId: thread.hostId,
    kind: thread.kind,
    status: { type: thread.status.type, activeFlags: [...flags] },
  };
  schemas.validate(
    { $defs: contract.$defs, $ref: "#/$defs/AppThreadObservation" } as Schema,
    observed,
    16384,
  );
  return observed;
}
/** Ordinary bundled MCP server, owned by the service. No Desktop pipe protocol,
 * debug hook, App Server process or fabricated live turn. The actor is an explicitly
 * assigned App-owned controller task. Domain journals own all mutation attempts. */
export class CodexAppTools {
  private client: Client | null = null;
  private actorVerified = false;
  private readonly settings: AppToolsSettings;
  private readonly inputSchemas = new Map<string, Schema>();
  private connecting: Promise<void> | null = null;
  private closed = false;
  private inspectionOnly = false;
  private connectedServer: string | null = null;
  get isClosed(): boolean { return this.closed; }
  get isCurrentInstallation(): boolean {
    if (!this.connectedServer) return true;
    try { return this.connectedServer === resolveAppToolsLaunch(this.settings).serverPath; }
    catch { return false; }
  }
  constructor(settings: AppToolsSettings) {
    schemas.validate(
      { $defs: contract.$defs, $ref: "#/$defs/AppToolsSettings" } as Schema,
      settings,
      131072,
    );
    requireThat(
      (!settings.nodeExecutable || isAbsolute(settings.nodeExecutable)) &&
        (!settings.serverPath || isAbsolute(settings.serverPath)) &&
        (settings.pipePath === "auto" ||
          settings.pipePath.startsWith("\\\\.\\pipe\\")),
      "invalid_arguments",
      "App Tools requires local installed paths and an explicit or automatically discovered Desktop pipe.",
    );
    this.settings = structuredClone(settings);
  }
  /** Explicit read-only installation setup verifies the current App Tools actor. */
  static async inspect(settings: AppToolsSettings): Promise<AppToolsSettings> {
    const client = new CodexAppTools(settings);
    client.inspectionOnly = true;
    try {
      await client.verifyActor();
      return structuredClone(settings);
    } finally {
      await client.close();
    }
  }
  private connect(): Promise<void> {
    requireThat(
      !this.closed,
      "app_tools_closed",
      "Original App Tools connection is closed.",
    );
    return (this.connecting ??= (async () => {
      const { serverPath, nodeExecutable } = resolveAppToolsLaunch(this.settings);
      requireThat(
        !this.closed,
        "app_tools_closed",
        "App Tools was closed before startup.",
      );
      let last: unknown = new IvyError(
        "app_tools_pipe_unavailable",
        "No current Codex Desktop App Tools pipe is available.",
      );
      const paths = this.pipePaths();
      let next = 0, connected = false;
      const findPipe = async (): Promise<void> => {
        while (!connected && next < paths.length) {
          const pipePath = paths[next++]!;
          const client = new Client({ name: "ivy-app-tools", version: "1.0.0" });
          const transport = new StdioClientTransport({
            command: nodeExecutable,
            args: [serverPath],
            cwd: dirname(serverPath),
            stderr: "ignore",
            maxBufferSize: 1024 * 1024,
            env: { CODEX_APP_TOOLS_PIPE_PATH: pipePath },
          });
          try {
            await client.connect(transport, { timeout: 30000 });
            const catalog = await client.listTools({}, { timeout: 30000 });
            requireThat(
              catalog.nextCursor === undefined,
              "app_tools_contract_changed",
              "A complete App Tools catalogue is required.",
            );
            const selected = new Map<string, Schema>();
            for (const name of runtimeNames) {
              const matches = catalog.tools.filter((tool) => tool.name === name);
              requireThat(
                matches.length === 1,
                "app_tools_contract_changed",
                "Required App Tools method is missing or duplicated.",
              );
              selected.set(name, matches[0]!.inputSchema as Schema);
            }
            if (connected) { await client.close(); return; }
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
            try { await client.close(); } catch {}
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
    })());
  }
  private pipePaths(): string[] {
    if (this.settings.pipePath !== "auto") return [this.settings.pipePath];
    const prefix = "codex-browser-use-",
      current = process.env["CODEX_APP_TOOLS_PIPE_PATH"];
    const names = new Set<string>();
    if (current?.startsWith("\\\\.\\pipe\\" + prefix)) names.add(current);
    try {
      for (const name of readdirSync("\\\\.\\pipe\\"))
        if (name.startsWith(prefix)) names.add("\\\\.\\pipe\\" + name);
    } catch {
      /* The final unavailable error is clearer than a platform-specific directory error. */
    }
    return [...names];
  }
  private async invoke(
    name: (typeof runtimeNames)[number],
    args: Record<string, unknown>,
    operationId: string,
    actorThreadId = this.settings.actorThreadId,
  ): Promise<unknown> {
    requireThat(
      uuid.test(operationId) && uuid.test(actorThreadId),
      "invalid_arguments",
      "Original App Tools operation and actor IDs required.",
    );
    await this.connect();
    schemas.validate(this.inputSchemas.get(name)!, args, 65536);
    const client = this.client;
    requireThat(
      client,
      "app_tools_closed",
      "App Tools connection is unavailable.",
    );
    const result = await client.callTool(
      {
        name,
        arguments: args,
        _meta: { thread_id: actorThreadId, call_id: operationId },
      },
      { timeout: 30000 },
    );
    const content = result.content;
    if (result.isError === true && name === "transfer_voice_call" &&
      Array.isArray(content) && content.length === 1 && content[0]?.type === "text" &&
      typeof content[0]["text"] === "string" &&
      /^(?:Error:\s*)?The current voice call could not be transferred to that task\.$/.test(
        content[0]["text"].trim()))
      throw new IvyError("app_tools_voice_not_ready", "Desktop Voice is not ready to transfer yet.");
    requireThat(
      result.isError !== true &&
        Array.isArray(content) &&
        content.length === 1 &&
        content[0]?.type === "text" &&
        typeof content[0]["text"] === "string" &&
        Buffer.byteLength(content[0]["text"]) <= 1024 * 1024,
      "app_tools_response_invalid",
      "App Tools did not return a bounded successful JSON result.",
    );
    return JSON.parse(content[0]["text"] as string) as unknown;
  }
  async readThread(threadId: string): Promise<AppThreadObservation> {
    requireThat(
      uuid.test(threadId),
      "invalid_arguments",
      "Exact App thread ID required.",
    );
    return appThreadObservation(
      await this.invoke(
        "read_thread",
        {
          threadId,
          hostId: "local",
          turnLimit: 1,
          includeOutputs: false,
          maxOutputCharsPerItem: 0,
        },
        randomUUID(),
      ),
      threadId,
    );
  }
  async verifyActor(): Promise<AppThreadObservation> {
    const actor = await this.readThread(this.settings.actorThreadId);
    this.actorVerified = true;
    return actor;
  }
  async navigateToThread(threadId: string): Promise<void> {
    requireThat(
      uuid.test(threadId),
      "invalid_arguments",
      "Exact App thread ID required.",
    );
    const result = (await this.invoke(
      "navigate_to_codex_page",
      { threadId },
      randomUUID(),
    )) as { navigated?: unknown };
    requireThat(
      result?.navigated === true,
      "app_tools_response_invalid",
      "App Tools did not confirm the requested local task was loaded.",
    );
  }
  /** Move the Voice session that Desktop just started into the prepared local task.
   * New Desktop builds may resume their most recent Voice task despite navigation. */
  async transferVoiceCall(sourceThreadId: string, threadId: string): Promise<void> {
    requireThat(!this.inspectionOnly && uuid.test(sourceThreadId) && uuid.test(threadId) &&
      sourceThreadId !== threadId,
      "invalid_arguments", "Exact source and destination Voice task IDs required.");
    // The caller verified this actor while preparing the task or opening this port.
    // Re-reading it here adds a failure point between Micro startup and call answer.
    requireThat(this.actorVerified, "app_tools_actor_unverified",
      "The assigned App Tools controller must be verified before Voice transfer.");
    try {
      const result = await this.invoke("transfer_voice_call", { threadId }, randomUUID(), sourceThreadId) as {
        transferred?: unknown; threadId?: unknown;
      };
      requireThat(result?.transferred === true && (result.threadId === undefined || result.threadId === threadId),
        'app_tools_response_invalid', 'Desktop did not confirm Voice transfer to the exact prepared task.');
    } catch (error) {
      if (IvyError.from(error).code === "app_tools_voice_not_ready") throw error;
      throw new IvyError("app_tools_transfer_unknown",
        "Desktop did not confirm the Voice transfer; observe its task before continuing.",
        "unknown", { causeCode: IvyError.from(error).code });
    }
  }
  async listThreads(limit = 50): Promise<AppThreadSummary[]> {
    requireThat(
      Number.isInteger(limit) && limit >= 1 && limit <= 50,
      "invalid_arguments",
      "Bounded App thread limit required.",
    );
    const value = (await this.invoke(
      "list_threads",
      { limit },
      randomUUID(),
    )) as {
      schemaVersion?: unknown;
      pinnedThreads?: unknown;
      threads?: unknown;
    };
    requireThat(
      value?.schemaVersion === 4 &&
        Array.isArray(value.pinnedThreads) &&
        Array.isArray(value.threads),
      "app_tools_response_invalid",
      "App thread catalogue is invalid.",
    );
    const threads = [...value.pinnedThreads, ...value.threads]
      .filter((thread): thread is AppThreadSummary => {
        const item = thread as Partial<AppThreadSummary> | null;
        return (
          item?.kind === "codex" &&
          item.hostId === "local" &&
          typeof item.id === "string" &&
          uuid.test(item.id) &&
          typeof item.status === "string"
        );
      })
      .map((thread) => ({
        id: thread.id,
        hostId: thread.hostId,
        kind: thread.kind,
        status: thread.status,
      }));
    schemas.validate(
      { $defs: contract.$defs, $ref: "#/$defs/AppThreadList" } as Schema,
      threads,
      65536,
    );
    return threads;
  }
  async internalProjectId(): Promise<string> {
    const root = resolve(this.settings.internalProjectRoot ?? join(accountHome(), ".ivy", "codex-projects"));
    const value = (await this.invoke("list_projects", {}, randomUUID())) as {
      schemaVersion?: unknown;
      projects?: unknown;
    };
    requireThat(value?.schemaVersion === 2 && Array.isArray(value.projects),
      "app_tools_response_invalid", "Desktop project catalogue is invalid.");
    const samePath = (path: string) => process.platform === "win32"
      ? resolve(path).toLowerCase() === root.toLowerCase()
      : resolve(path) === root;
    const project = value.projects.find((entry: unknown) => {
      const item = entry as { projectId?: unknown; projectKind?: unknown; hostId?: unknown; path?: unknown };
      return item?.projectKind === "local" && item.hostId === "local" &&
        typeof item.path === "string" && samePath(item.path) &&
        typeof item.projectId === "string" && uuid.test(item.projectId);
    }) as { projectId: string } | undefined;
    requireThat(project, "app_tools_internal_project_missing",
      `Register ${root} as a local Codex Desktop project before PhoneBridge creates a Voice task.`);
    return project.projectId;
  }
  /** The App creates a user-owned local task. A lost acknowledgement cannot authorize
   * another creation; the caller journals the original attempt before submission. */
  async createLocalThread(
    prompt: string,
    selection: { model: string; reasoningEffort: string },
    operationId: string,
    beforeSubmit: () => Promise<void>,
  ): Promise<string> {
    requireThat(!this.inspectionOnly && prompt.length > 0 && prompt.length <= 4096,
      "invalid_arguments", "A bounded task creation prompt is required.");
    await this.connect();
    await this.verifyActor();
    const args = { prompt, target: { type: "projectless" }, title: "PhoneBridge Voice",
      model: selection.model, thinking: selection.reasoningEffort };
    schemas.validate(this.inputSchemas.get("create_thread")!, args, 65536);
    await beforeSubmit();
    let value: unknown;
    try { value = await this.invoke("create_thread", args, operationId); }
    catch { throw new IvyError("app_tools_create_unknown", "Original Desktop task creation result is unknown; do not create another task.", "unknown"); }
    const result = value as { threadId?: unknown; hostId?: unknown } | null;
    if (!(result && typeof result.threadId === "string" && uuid.test(result.threadId) && result.hostId === "local"))
      throw new IvyError("app_tools_create_unknown", "Desktop did not confirm one exact local task; do not create another task.", "unknown");
    return result.threadId;
  }
  /** A successful MCP result is the App-owned acknowledgement. The caller records intent
   * before submission and must treat every later failure as unknown. */
  async sendMessage(
    threadId: string,
    prompt: string,
    operationId: string,
    beforeSubmit: () => Promise<void>,
    selection: { model?: string; reasoningEffort?: string } = {},
  ): Promise<{ threadId: string; sent: true }> {
    requireThat(
      !this.inspectionOnly,
      "app_tools_read_only",
      "Installation inspection cannot send task messages.",
    );
    requireThat(
      uuid.test(threadId) &&
        uuid.test(operationId) &&
        threadId !== this.settings.actorThreadId &&
        prompt.length > 0 &&
        prompt.length <= 4096,
      "invalid_arguments",
      "Exact prompt operation and target task required.",
    );
    await this.connect();
    if (!this.actorVerified) await this.verifyActor();
    const args = {
      threadId,
      hostId: "local",
      prompt,
      ...(selection.model ? { model: selection.model } : {}),
      ...(selection.reasoningEffort ? { thinking: selection.reasoningEffort } : {}),
    };
    schemas.validate(
      this.inputSchemas.get("send_message_to_thread")!,
      args,
      65536,
    );
    await beforeSubmit();
    try {
      await this.invoke("send_message_to_thread", args, operationId);
      return { threadId, sent: true };
    } catch {
      throw new IvyError(
        "app_tools_prompt_unknown",
        "Original Desktop prompt result is unknown; do not repeat it.",
        "unknown",
      );
    }
  }
  /** Caller durably records intent immediately before this callback permits submission.
   * All exceptions after the callback are unknown; no retry or replacement operation. */
  async archiveThread(
    threadId: string,
    operationId: string,
    beforeSubmit: () => Promise<void>,
  ): Promise<{ threadId: string; archived: true }> {
    requireThat(
      !this.inspectionOnly,
      "app_tools_read_only",
      "Installation inspection cannot archive tasks.",
    );
    requireThat(
      uuid.test(threadId) &&
        uuid.test(operationId) &&
        threadId !== this.settings.actorThreadId,
      "invalid_arguments",
      "Exact archive operation/target required; target cannot be the assigned controller task.",
    );
    await this.connect();
    await this.verifyActor();
    schemas.validate(
      this.inputSchemas.get("set_thread_archived")!,
      { threadId, hostId: "local", archived: true },
      65536,
    );
    await beforeSubmit();
    try {
      const result = (await this.invoke(
        "set_thread_archived",
        { threadId, hostId: "local", archived: true },
        operationId,
      )) as { threadId?: unknown; archived?: unknown };
      requireThat(
        result?.threadId === threadId && result.archived === true,
        "app_tools_response_invalid",
        "Archive acknowledgement changed target or outcome.",
      );
      return { threadId, archived: true };
    } catch {
      throw new IvyError(
        "app_tools_archive_unknown",
        "Original Desktop archive result is unknown; do not repeat it.",
        "unknown",
      );
    }
  }
  async close(): Promise<void> {
    this.closed = true;
    const client = this.client;
    this.client = null;
    this.connectedServer = null;
    if (client) await client.close();
  }
}
