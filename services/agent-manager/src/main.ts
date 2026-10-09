import { dirname, join } from "node:path";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import { NativeJournal } from "./journal.js";
import { LocalAgentState } from "./hive-state.js";
import type { SavedState } from "./hive-state.js";
import { NativeOperations } from "./operations.js";
import { NativeInteractions } from "./interactions.js";
import { NativeNotifications, NativeNoticePolicy, nativeBrowserNotice, inputBrowserNotice } from "./notifications.js";
import { publishBrowserNotice } from "../../../packages/sdk/src/node.js";
import { answerNativeClock, nativeClockPrincipal } from "./clock.js";
import {
  nativeFrameBytes,
  nativeRequestFrameBytes,
  nativeAnswerFrameBytes,
  managementFrameBytes,
} from "./limits.js";
import { startNativeProcess } from "./process.js";
import type { NativeProcessOptions } from "./process.js";
import { agentRegistry } from "./registry.js";
import { discoverNativeTools } from "./native-discovery.js";
import { InstructionsManager } from "./instructions.js";
import { McpConfigurationManager, SkillsManager } from "./environment.js";
import { ClaudeMcpConfigurationManager } from "./claude-mcp.js";
import { loadEnvironmentDefaults } from "./environment-defaults.js";
import { ProjectLocations } from "./projects.js";
import { listDirectories } from "./directories.js";
import {
  NativeInventory,
  publishNativeProjects,
  publishNativeThreads,
} from "./inventory.js";
import {
  canonical,
  hashJson,
  HiveClient,
  isReadOnlyNativeMethod,
  IvyError,
  requireThat,
  ServiceClient,
  toolDefinitionHash,
  validateAgent,
} from "../../../packages/sdk/src/node.js";
import type {
  Agent,
  BrowserNotice,
  InvocationContext,
  JsonToolHandler,
  ServiceConnection,
  ToolHandler,
  Wire,
} from "../../../packages/sdk/src/node.js";
import {
  configurationPath,
  HealthFile,
  instanceConfig,
} from "../../../packages/sdk/src/host.js";
import { applyAgentRuntimeReset } from "./runtime-reset.js";
import { ownedProcessStops } from "./process-evidence.js";
import { stageFile } from "./staged-files.js";

type NativeOwner = Pick<
  Awaited<ReturnType<typeof startNativeProcess>>,
  "rpc" | "catalog" | "epoch" | "initialized" | "launcherPid" | "close"
> & {
  windowsShell?: Agent.WindowsShellStatus | null;
  nativeHome?: string;
  ownsNativeHome?: boolean;
  connectionMode?: "owned-stdio" | "external-proxy";
  actualServerVersion?: string;
  socketPath?: string | null;
  daemon?: Awaited<ReturnType<typeof startNativeProcess>>["daemon"];
};
export type NativeLauncher = (
  options: NativeProcessOptions,
) => Promise<NativeOwner>;

function outcome(value: Agent.Operation): Wire.Json {
  if (value.reply && "result" in value.reply) return value.reply.result;
  if (value.reply && "error" in value.reply)
    throw new IvyError(
      "native_error",
      "Native method returned its original error; inspect this caller operation.",
      "completed",
      { operationId: value.operationId, nativeError: value.reply.error },
    );
  throw new IvyError(
    value.code ?? "native_operation_pending",
    "Inspect the original native operation; it is never resent under this identity.",
    value.phase === "failed" ? "not_executed" : "unknown",
    { operationId: value.operationId, phase: value.phase },
  );
}

/** A Hive reconnection replaces only its generation. Native failure ends this independently supervised owner. */
export async function startAgentManager(
  path: string,
  launchNative: NativeLauncher = startNativeProcess,
): Promise<{
  service: ServiceClient;
  closed: Promise<string>;
  close: () => Promise<void>;
}> {
  const config = await instanceConfig(path);
  const settings = config.settings as unknown as Agent.Settings;
  validateAgent("Settings", settings);
  requireThat(
    config.componentId === "agent-manager" && config.credential,
    "invalid_arguments",
    "AgentManager requires its own component identity and Hive credential.",
  );
  await applyAgentRuntimeReset(config.dataRoot, settings);
  const journal = new NativeJournal(
    {
      serviceNodeId: config.serviceNodeId,
      hostId: config.hostId,
      nativeVersion: settings.nativeVersion,
      nativeExecutableHash: settings.nativeExecutableHash,
    },
    settings.limits,
  );
  const notifications = new NativeNotifications(
    {
      serviceNodeId: config.serviceNodeId,
      nativeVersion: settings.nativeVersion,
    },
    journal.reserveNotificationSequence(),
    settings.limits.maxNotificationBytes,
  );
  const noticePolicy = new NativeNoticePolicy();
  let sendBrowserNotice: ((threadId: string, notice: BrowserNotice) => Promise<void>) | null = null;
  const health = new HealthFile(config),
    work = new Set<Promise<unknown>>();
  let native: NativeOwner | null = null,
    service: ServiceClient | null = null,
    closing = false,
    stopWork: Promise<void> | null = null;
  let hiveConnection: ServiceConnection | null = null;
  let requestThreadRefresh: (() => void) | null = null,
    requestProjectRefresh: (() => void) | null = null;
  const state = new LocalAgentState(
    join(config.dataRoot, "journal", "agent-runtime.sqlite"),
    config.serviceNodeId,
    settings.limits,
  );
  let capabilityState: SavedState<Agent.CapabilityProfile> | null =
    await state.read("capabilities", "profile");
  if (!capabilityState) {
    const capabilities = structuredClone(settings.capabilities ?? []);
    capabilityState = await state.write("capabilities", "profile", {
      serviceNodeId: config.serviceNodeId,
      hostId: config.hostId,
      revision: 1,
      capabilities,
      updatedAt: new Date().toISOString(),
      operationId: null,
    } satisfies Agent.CapabilityProfile);
  }
  const operations = new NativeOperations(state, journal);
  let instructions: InstructionsManager | null = null,
    mcp: McpConfigurationManager | ClaudeMcpConfigurationManager | null = null,
    skills: SkillsManager | null = null;
  let environmentAt = 0,
    locations: ProjectLocations | null = null;
  const startupClocks: Agent.PendingInput[] = [];
  let healthWork: Promise<void> = Promise.resolve(),
    timer: NodeJS.Timeout | null = null,
    controlBusy = false;
  let finishClosed!: (code: string) => void;
  const closed = new Promise<string>((resolve) => {
    finishClosed = resolve;
  });
  const track = <T>(promise: Promise<T>): Promise<T> => {
    work.add(promise);
    void promise.then(
      () => work.delete(promise),
      () => work.delete(promise),
    );
    return promise;
  };
  const managedSkillsRoot = (nativeHome: string) =>
    settings.skillsRoot ??
    (settings.appServer?.mode === "claude-adapter"
      ? join(config.dataRoot, "claude-config", "skills")
      : join(dirname(nativeHome), ".agents", "skills"));
  const managedMcp = (nativeHome: string, defaults: Agent.McpDocument) =>
    settings.appServer?.mode === "claude-adapter"
      ? new ClaudeMcpConfigurationManager(
          join(config.dataRoot, "claude-mcp.json"),
          config.hostId,
          config.publicBaseUrl,
          config.credential!,
          defaults,
        )
      : new McpConfigurationManager(
          nativeHome,
          config.hostId,
          config.publicBaseUrl,
          config.credential!,
          defaults,
        );
  journal.onTerminal = (identity) => {
    if (identity.callerPrincipalId !== nativeClockPrincipal)
      void track(operations.settle(identity)).catch((error) => {
        void writeHealth(
          false,
          null,
          "Hive receipt could not be saved: " + IvyError.from(error).code,
        ).catch(() => undefined);
      });
  };
  const writeHealth = (
    ready: boolean,
    generation: number | null,
    message: string,
  ) => {
    const active = journal.status();
    const warning =
      active.retained >= active.maximum * 0.8 ||
      active.bytes >= active.maximumBytes * 0.8
        ? " Connection memory near capacity."
        : "";
    healthWork = healthWork
      .catch(() => undefined)
      .then(() => health.write(ready, message + warning, generation));
    return healthWork;
  };
  const close = (code = "agent_stopped"): Promise<void> => {
    if (!stopWork) {
      closing = true;
      if (timer) clearInterval(timer);
      stopWork = (async () => {
        try {
          await native?.close();
          await Promise.allSettled([...work]);
          await service?.stop();
          await Promise.all([
            instructions?.close(),
            mcp?.close(),
            skills?.close(),
          ]);
          locations?.close();
          state.close();
          await healthWork.catch(() => undefined);
          await health.write(false, "AgentManager stopped: " + code + ".");
        } catch (error) {
          code = "agent_shutdown_failed";
          throw error;
        } finally {
          try {
            journal.close();
          } finally {
            finishClosed(code);
          }
        }
      })();
    }
    return stopWork;
  };
  const fail = (code: string) => {
    if (!closing) void close(code).catch(() => undefined);
  };
  try {
    await writeHealth(
      false,
      null,
      "Starting the pinned public native process in its own home.",
    );
    const defaults = await loadEnvironmentDefaults(
      config.artifactRoot,
      config.publicBaseUrl,
    );
    const bootstrap = new HiveClient(config.publicBaseUrl, {
      credential: config.credential,
    });
    native = await launchNative({
      artifactRoot: config.artifactRoot,
      dataRoot: config.dataRoot,
      settings,
      clientVersion: config.version,
      hiveCredential: config.credential,
      beforeLaunch: async (home) => {
        if (settings.patcherEnabled === false) return;
        instructions = new InstructionsManager(
          home,
          config.hostId,
          config.publicBaseUrl,
          defaults.instructions,
        );
        mcp = managedMcp(home, defaults.mcp);
        skills = new SkillsManager(
          home,
          managedSkillsRoot(home),
          config.hostId,
          config.publicBaseUrl,
          defaults.skills,
        );
        // A first installation must be useful even while Hive is temporarily unavailable.
        // Stored global/host overrides, when reachable, are still applied before Codex starts.
        await Promise.all([
          instructions.bootstrap(),
          mcp.bootstrap(),
          skills.bootstrap(),
        ]);
        await Promise.all([
          instructions.synchronize(bootstrap),
          mcp.synchronize(bootstrap),
          skills.synchronize(bootstrap),
        ]);
        if (settings.appServer?.mode === "claude-adapter")
          requireThat(
            mcp.status.state === "applied" || mcp.status.state === "disabled",
            "native_environment_unavailable",
            "Claude adapter MCP configuration was not applied.",
          );
        environmentAt = Date.now();
      },
      beginEpoch: (epoch) => journal.beginEpoch(epoch),
      onClose: (epoch, code) => journal.loseEpoch(epoch, code),
      onRequest: (epoch, value) => {
        const input = journal.observeInput(
          { serviceNodeId: config.serviceNodeId, epoch, requestId: value.id },
          value.method,
          value.params,
        );
        if (value.method === "currentTime/read") {
          if (native) answerNativeClock(journal, native.rpc, input);
          else startupClocks.push(input);
        } else if (service?.ready && !closing) {
          const notice = inputBrowserNotice(input);
          if (notice) void sendBrowserNotice?.(input.threadId!, notice).catch(() => undefined);
          try {
            service.connection.notification("agent", "inputs", "1.0.0", {
              identity: input.identity,
              threadId: input.threadId,
            });
          } catch {
            /* The next connection refresh reads current pending inputs. */
          }
        }
      },
      onNotification: (epoch, value) => {
        noticePolicy.observe(value);
        const event = notifications.observe(epoch, value.method, value.params);
        if (service?.ready && !closing) {
          const notice = nativeBrowserNotice(config.serviceNodeId, event);
          if (notice) void sendBrowserNotice?.((value.params as { threadId: string }).threadId, notice).catch(() => undefined);
          try {
            service.connection.notification(
              "agent",
              "notification",
              "1.0.0",
              event,
            );
          } catch {
            /* History recovery remains available. */
          }
        }
        if (value.method === "serverRequest/resolved") {
          const params = value.params as {
            requestId: Agent.RequestId;
            threadId: string;
          };
          journal.resolveInput(epoch, params.requestId, params.threadId);
        }
        if (
          [
            "thread/started",
            "thread/name/updated",
            "thread/project/updated",
            "thread/archived",
            "thread/unarchived",
            "thread/deleted",
            "thread/status/changed",
            "turn/started",
            "turn/completed",
          ].includes(value.method) &&
          !(["thread/status/changed", "turn/started", "turn/completed"].includes(value.method) &&
            noticePolicy.isInternal((value.params as { threadId: string }).threadId) === true)
        )
          requestThreadRefresh?.();
        if (value.method === "project/changed") {
          requestProjectRefresh?.();
          requestThreadRefresh?.();
        }
        // Model events are transient; authoritative recovery uses native turn history.
      },
    });
    locations = new ProjectLocations(
      config.dataRoot,
      settings,
      [
        config.artifactRoot,
        ...(native.nativeHome ? [native.nativeHome] : []),
        ...(config.workRoot ? [config.workRoot] : []),
        ...(config.logsRoot ? [config.logsRoot] : []),
      ],
      async ({ name, cwd, idempotencyKey }) => {
        const reply = await native!.rpc.request("project/create", { name, roots: [{ path: cwd }], idempotencyKey });
        if ("error" in reply) throw new IvyError("native_error", reply.error.message, "not_executed");
        await freshProjects(service!.connection);
        const id = (reply.result as { project: { id: string } }).project.id;
        const project = projectSnapshot.projects.projects.find(value => value.nativeId === id);
        requireThat(project, "native_inventory_changed", "The created Codex project is not in the current inventory.");
        return structuredClone(project);
      },
      state,
    );
    const owner = native,
      catalogHash = hashJson(owner.catalog),
      registry = agentRegistry(owner.catalog),
      inventory = new NativeInventory(owner.rpc, owner.epoch);
    const interactions = new NativeInteractions(owner.rpc, owner.epoch);
    if (
      settings.patcherEnabled !== false &&
      owner.nativeHome &&
      !instructions
    ) {
      instructions = new InstructionsManager(
        owner.nativeHome,
        config.hostId,
        config.publicBaseUrl,
        defaults.instructions,
      );
      mcp = managedMcp(owner.nativeHome, defaults.mcp);
      skills = new SkillsManager(
        owner.nativeHome,
        managedSkillsRoot(owner.nativeHome),
        config.hostId,
        config.publicBaseUrl,
        defaults.skills,
      );
    }
    for (const input of startupClocks.splice(0))
      answerNativeClock(journal, owner.rpc, input);
    const nativeDefinitions = new Map(
      registry.namespaces[0]!.tools.map((definition) => [
        definition.nativeMethod,
        toolDefinitionHash(definition),
      ]),
    );
    void owner.rpc.closed.then((code) => fail(code));
    let projectSnapshot = inventory.currentProjects(), projectObserved = false;
    let threadRefreshAt = 0,
      projectRefreshAt = 0,
      threadRefreshWork: Promise<void> | null = null,
      projectRefreshWork: Promise<void> | null = null;
    let threadRefreshQueued = false,
      projectRefreshQueued = false,
      threadInventoryCode: string | null = null,
      projectInventoryCode: string | null = null;
    const refreshThreads = (connection: ServiceConnection): Promise<void> => {
      if (threadRefreshWork) {
        threadRefreshQueued = true;
        return threadRefreshWork;
      }
      const task = track(
        (async () => {
          connection.signal.throwIfAborted();
          const noticeRevision = noticePolicy.revision;
          const next = await inventory.collectThreads(connection.signal);
          noticePolicy.setThreads(next.threads, noticeRevision);
          await publishNativeThreads(
            connection,
            journal,
            next,
            connection.signal,
          );
          connection.signal.throwIfAborted();
          threadInventoryCode = null;
        })(),
      );
      threadRefreshWork = task;
      threadRefreshAt = Date.now();
      void task.then(
        () => {
          if (threadRefreshWork !== task) return;
          threadRefreshWork = null;
          const queued = threadRefreshQueued;
          threadRefreshQueued = false;
          if (
            queued &&
            hiveConnection &&
            !hiveConnection.signal.aborted &&
            !closing
          )
            void refreshThreads(hiveConnection).catch(() => undefined);
        },
        (error) => {
          if (threadRefreshWork !== task) return;
          threadRefreshWork = null;
          threadInventoryCode = IvyError.from(error).code;
          const queued = threadRefreshQueued;
          threadRefreshQueued = false;
          if (
            queued &&
            hiveConnection &&
            !hiveConnection.signal.aborted &&
            !closing
          )
            void refreshThreads(hiveConnection).catch(() => undefined);
        },
      );
      return task;
    };
    const refreshProjects = (
      connection: ServiceConnection,
      nativeRead = true,
    ): Promise<void> => {
      if (projectRefreshWork) {
        projectRefreshQueued = projectRefreshQueued || nativeRead;
        return projectRefreshWork;
      }
      const task = track(
        (async () => {
          connection.signal.throwIfAborted();
          const next = nativeRead
            ? await inventory.collectProjects(connection.signal)
            : inventory.currentProjects();
          noticePolicy.setProjects(next.projects.projects);
          locations!.setNativeProjects(next.projects.projects);
          await publishNativeProjects(
            connection,
            journal,
            next,
            connection.signal,
          );
          connection.signal.throwIfAborted();
          projectSnapshot = next;
          projectObserved = true;
          projectInventoryCode = null;
        })(),
      );
      projectRefreshWork = task;
      projectRefreshAt = Date.now();
      void task.then(
        () => {
          if (projectRefreshWork !== task) return;
          projectRefreshWork = null;
          const queued = projectRefreshQueued;
          projectRefreshQueued = false;
          if (
            queued &&
            hiveConnection &&
            !hiveConnection.signal.aborted &&
            !closing
          )
            void refreshProjects(hiveConnection).catch(() => undefined);
        },
        (error) => {
          if (projectRefreshWork !== task) return;
          projectRefreshWork = null;
          projectInventoryCode = IvyError.from(error).code;
          const queued = projectRefreshQueued;
          projectRefreshQueued = false;
          if (
            queued &&
            hiveConnection &&
            !hiveConnection.signal.aborted &&
            !closing
          )
            void refreshProjects(hiveConnection).catch(() => undefined);
        },
      );
      return task;
    };
    const freshProjects = async (connection: ServiceConnection) => {
      if (projectRefreshWork) await projectRefreshWork;
      await refreshProjects(connection);
    };
    const noticeReads = new Map<string, Promise<void>>();
    sendBrowserNotice = async (threadId, notice) => {
      if (!service?.ready || closing) return;
      // Initial discovery and project changes share their existing read; never poll for badges.
      if (projectRefreshWork) await projectRefreshWork;
      else if (!projectObserved) await refreshProjects(service.connection);
      if (noticePolicy.isInternal(threadId) === null) {
        let read = noticeReads.get(threadId);
        if (!read) {
          const revision = noticePolicy.revision;
          read = (async () => {
            const reply = await owner.rpc.request("thread/read", { threadId, includeTurns: false }, { detachOnTimeout: true });
            if ("error" in reply) throw new IvyError("native_inventory_unavailable", "Cannot determine the task's notification policy.");
            noticePolicy.setThread(threadId, (reply.result as { thread: Record<string, Wire.Json> }).thread, revision);
          })().finally(() => noticeReads.delete(threadId));
          noticeReads.set(threadId, read);
        }
        await read;
      }
      if (noticePolicy.isInternal(threadId) === false && service?.ready && !closing)
        await publishBrowserNotice(service.connection, "agent", notice);
    };
    requestThreadRefresh = () => {
      if (service?.ready && hiveConnection)
        void refreshThreads(hiveConnection).catch(() => undefined);
    };
    requestProjectRefresh = () => {
      if (service?.ready && hiveConnection)
        void refreshProjects(hiveConnection).catch(() => undefined);
    };
    const identity = (
      context: InvocationContext,
      operationId = context.operationId,
    ) => {
      requireThat(
        operationId,
        "operation_id_required",
        "Native requests require a stable caller operationId.",
      );
      requireThat(
        context.callerPrincipalId !== nativeClockPrincipal,
        "native_reserved_principal",
        "The native host clock journal identity is reserved for local protocol responses.",
      );
      return { callerPrincipalId: context.callerPrincipalId, operationId };
    };
    const available = () =>
      requireThat(
        !closing && owner.rpc.connected && journal.epoch === owner.epoch,
        "native_unavailable",
        "The current native owner is unavailable.",
      );
    const processStops = await ownedProcessStops(config.dataRoot);
    const status = (): Agent.Status => ({
      serviceNodeId: config.serviceNodeId,
      hostId: config.hostId,
      serverType: settings.appServer?.mode === 'claude-adapter' ? 'claude' : 'codex',
      nativeVersion: settings.nativeVersion,
      nativeExecutableHash: settings.nativeExecutableHash,
      catalogHash,
      epoch: journal.epoch,
      processStops,
      // Windows uses a Job launcher; its PID is not falsely presented as the native child's PID.
      pid: process.platform === "win32" ? null : owner.launcherPid,
      state: closing
        ? "stopping"
        : owner.rpc.connected
          ? service?.ready
            ? "ready"
            : "initializing"
          : "failed",
      observedAt: new Date().toISOString(),
      code: threadInventoryCode ?? projectInventoryCode,
      initialized: owner.initialized,
      pendingInputs: journal.pendingInputCount(),
      operations: journal.status(),
      observedMethods: journal.observedMethods(),
      windowsShell: owner.windowsShell ?? null,
      capabilities: capabilityState!.value,
      ...(instructions ? { instructions: instructions.status } : {}),
      ...(mcp && skills
        ? { environment: { mcp: mcp.status, skills: skills.status } }
        : {}),
      ...(owner.nativeHome
        ? {
            connection: {
              mode: owner.connectionMode ?? "owned-stdio",
              ownsServer: owner.connectionMode !== "external-proxy",
              ownsHome: owner.ownsNativeHome ?? false,
              actualHome: owner.nativeHome,
              actualVersion:
                owner.actualServerVersion ?? settings.nativeVersion,
              socketPath: owner.socketPath ?? null,
              serverPid: owner.daemon?.pid ?? null,
              lifecycle:
                owner.daemon?.status ??
                (owner.connectionMode === "external-proxy"
                  ? "explicit-endpoint"
                  : "owned-stdio"),
              mcpIdentity:
                owner.connectionMode === "external-proxy"
                  ? "shared-user-home"
                  : "instance-environment",
            },
          }
        : {}),
    });
    const executeNative = async (
      method: string,
      args: Wire.Json,
      context: InvocationContext,
    ): Promise<Agent.Operation> => {
      const key = identity(context);
      return operations.run(key, method, args, async () => {
        try {
          available();
          await owner.rpc.request(method, args, {
            requestId: operations.requestId(key),
            beforeSend: (id) => {
              journal.dispatch(key, owner.epoch, id);
            },
            beforeResolve: (id, reply) => {
              journal.finish(key, owner.epoch, id, reply);
            },
          });
        } catch (error) {
          if (journal.get(key)?.phase === "accepted")
            journal.rejectBeforeDispatch(key, IvyError.from(error).code);
        }
        return journal.get(key)!;
      });
    };
    const executeNativeRead = async (
      method: string,
      args: Wire.Json,
      context: InvocationContext,
    ): Promise<Wire.Json> => {
      context.signal.throwIfAborted();
      available();
      const reply = await owner.rpc.request(method, args, {
        detachOnTimeout: true,
        priority: "interaction",
      });
      context.signal.throwIfAborted();
      available();
      if ("error" in reply)
        throw new IvyError(
          "native_error",
          "Native read returned its original error.",
          "completed",
          { nativeError: reply.error },
        );
      return reply.result;
    };
    const handlers: Record<string, ToolHandler> = {
      "agent.status": () => status(),
      "agent.capabilities": () => structuredClone(capabilityState!.value),
      "agent.environmentDefaults": () => structuredClone(defaults),
      "agent.configureCapabilities": (args, context) =>
        track(
          (async () => {
            validateAgent("ConfigureCapabilitiesInput", args);
            const request = args as Agent.ConfigureCapabilitiesInput;
            requireThat(
              context.operationId === request.operationId,
              "mutation_conflict",
              "Tool and capability operation IDs must match.",
            );
            const capabilities = request.capabilities.map((value) => ({
              key: value.key,
              label: value.label.trim(),
            }));
            requireThat(
              new Set(capabilities.map((value) => value.key)).size ===
                capabilities.length &&
                capabilities.every((value) => value.label.length > 0),
              "invalid_arguments",
              "Capability keys must be unique and labels nonempty.",
            );
            if (capabilityState!.value.operationId === request.operationId) {
              requireThat(
                hashJson(capabilityState!.value.capabilities) ===
                  hashJson(capabilities),
                "mutation_conflict",
                "Capability operation was already used for another profile.",
              );
              return structuredClone(capabilityState!.value);
            }
            requireThat(
              capabilityState!.value.revision === request.expectedRevision,
              "revision_conflict",
              "Capability profile changed; reload its current revision.",
            );
            const next: Agent.CapabilityProfile = {
              serviceNodeId: config.serviceNodeId,
              hostId: config.hostId,
              revision: request.expectedRevision + 1,
              capabilities,
              updatedAt: new Date().toISOString(),
              operationId: request.operationId,
            };
            capabilityState = await state.write(
              "capabilities",
              "profile",
              next,
              capabilityState!.pin,
            );
            if (service?.ready)
              service.connection.notification(
                "agent",
                "capabilities",
                "1.0.0",
                next,
              );
            return structuredClone(next);
          })(),
        ),
      "agent.resolveWorkspace": (args, context) =>
        track(
          (async () => {
            validateAgent("WorkspaceResolveInput", args);
            available();
            const request = args as Agent.WorkspaceResolveInput;
            await freshProjects(service!.connection);
            if (request.prepare)
              requireThat(
                context.operationId,
                "operation_id_required",
                "Preparing a Task workspace requires a stable operation identity.",
              );
            return locations!.workspace(
              request.taskKey,
              request.requirement,
              request.prepare,
              config.hostId,
              config.serviceNodeId,
              request.verifyOrigin ?? false,
            );
          })(),
        ),
      "agent.listDirectories": (args) =>
        track((async () => {
          validateAgent("DirectoryListInput", args);
          available();
          return listDirectories(args as Agent.DirectoryListInput);
        })()),
      "agent.resolveProject": (args, context) =>
        track(
          (async () => {
            validateAgent("ProjectResolveInput", args);
            available();
            if ((args as Agent.ProjectResolveInput).selection.kind !== "projectless")
              await freshProjects(service!.connection);
            const result = await locations!.resolve(
              (args as Agent.ProjectResolveInput).selection,
              context.callerPrincipalId,
              (args as Agent.ProjectResolveInput).expectedProjectId,
            );
            return result;
          })(),
        ),
      "agent.catalog": () => {
        available();
        return structuredClone(owner.catalog);
      },
      "agent.discover": (args) => {
        available();
        return discoverNativeTools(
          owner.catalog,
          registry.namespaces[0]!.tools,
          args as Agent.NativeDiscoveryInput,
        );
      },
      "agent.stageFile": (args) =>
        track(
          (async () => {
            validateAgent("StageFileInput", args);
            return stageFile(
              join(config.dataRoot, "staged-files"),
              args as Agent.StageFileInput,
            );
          })(),
        ),
      "agent.frameLimits": (): Agent.FrameLimits => ({
        serviceNodeId: config.serviceNodeId,
        nativeVersion: owner.catalog.version,
        nativeExecutableHash: owner.catalog.nativeExecutableHash,
        catalogHash,
        epoch: journal.epoch,
        requestFrameBytes: nativeRequestFrameBytes,
        receivedFrameBytes: nativeFrameBytes,
        answerFrameBytes: nativeAnswerFrameBytes,
        managementFrameBytes,
      }),
      "agent.prevent": (args, context) => {
        available();
        const request = args as Agent.PreventInput;
        requireThat(
          !context.operationId || context.operationId === request.operationId,
          "mutation_conflict",
          "Outer prevention identity must match the original native operation.",
        );
        requireThat(
          request.nativeVersion === owner.catalog.version,
          "native_version_mismatch",
          "Prevention requires the exact installed native version.",
        );
        requireThat(
          nativeDefinitions.has(request.method) &&
            nativeDefinitions.get(request.method) ===
              request.expectedDefinitionHash,
          "native_definition_mismatch",
          "Prevention requires the installed native method definition.",
        );
        return operations.run(
          identity(context, request.operationId),
          request.method,
          request.params,
          async () => {
            throw new Error("Prevent cannot dispatch.");
          },
          true,
        );
      },
      "agent.invoke": (args, context) =>
        track(
          (async () => {
            const request = args as Agent.PreventInput;
            requireThat(
              request.operationId === context.operationId &&
                request.nativeVersion === owner.catalog.version &&
                nativeDefinitions.has(request.method) &&
                nativeDefinitions.get(request.method) ===
                  request.expectedDefinitionHash,
              "native_definition_mismatch",
              "Dispatch requires its bound native method and original operation identity.",
            );
            return executeNative(request.method, request.params, context);
          })(),
        ),
      "agent.interact": (args, context) =>
        track(
          (async () => {
            available();
            const request = args as Agent.PreventInput;
            requireThat(
              request.operationId === context.operationId &&
                request.nativeVersion === owner.catalog.version &&
                nativeDefinitions.get(request.method) ===
                  request.expectedDefinitionHash,
              "native_definition_mismatch",
              "Interaction requires its bound native method and original identity.",
            );
            const key = identity(context);
            return interactions.run(
              key.callerPrincipalId,
              key.operationId,
              request.method,
              request.params,
            );
          })(),
        ),
      "agent.interaction": (args, context) =>
        interactions.lookup(
          context.callerPrincipalId,
          String(args["operationId"]),
        ),
      "agent.read": (args, context) =>
        track(
          (async () => {
            available();
            const request = args as Agent.ReadInput;
            requireThat(
              [
                "thread/read",
                "thread/turns/list",
                "thread/items/list",
              ].includes(request.method),
              "invalid_arguments",
              "Read-only access cannot dispatch native mutations.",
            );
            requireThat(
              request.nativeVersion === owner.catalog.version,
              "native_version_mismatch",
              "Read requires the exact installed native version.",
            );
            const requestHash = hashJson({
              method: request.method,
              params: request.params,
            });
            let requestId: Agent.RequestId | null = null;
            const reply = await owner.rpc.request(
              request.method,
              request.params,
              {
                detachOnTimeout: true,
                priority: "interaction",
                beforeSend: (id) => {
                  requestId = id;
                },
              },
            );
            available();
            requireThat(
              requestId !== null,
              "native_observation_invalid",
              "Native observation is missing its dispatched request identity.",
            );
            const observation: Agent.ReadObservation = {
              schemaVersion: 1,
              observationId: randomUUID(),
              callerPrincipalId: context.callerPrincipalId,
              serviceNodeId: config.serviceNodeId,
              epoch: owner.epoch,
              nativeExecutableHash: owner.catalog.nativeExecutableHash,
              catalogHash,
              requestId,
              requestHash,
              observedAt: new Date().toISOString(),
              ...request,
              reply,
            };
            return observation;
          })(),
        ),
      "agent.operation": async (args, context) => {
        const operationId = String(args["operationId"]),
          value = await operations.read(identity(context, operationId));
        if (!value)
          throw new IvyError(
            "not_found",
            "Native operation was not found for this caller.",
            "not_executed",
            {
              kind: "agent_operation_absent",
              operationId,
              serviceNodeId: config.serviceNodeId,
              epoch: journal.epoch,
            } satisfies Agent.OperationAbsence,
          );
        return value;
      },
      "agent.inputs": (args) => journal.inputs(args as Agent.PendingInputQuery),
      "agent.inputDefinition": (args) => {
        available();
        const input = journal.input(
          (args as Agent.InputDefinitionQuery).identity,
        );
        requireThat(
          input &&
            input.identity.serviceNodeId === config.serviceNodeId &&
            input.identity.epoch === owner.epoch &&
            input.state === "pending",
          "native_input_expired",
          "Only an input confirmed pending on the current native owner has an answer definition.",
        );
        const definition = owner.catalog.serverRequests.find(
          (value) => value.method === input.method,
        );
        requireThat(
          definition,
          "native_method_unavailable",
          "The installed catalog does not define this native response.",
        );
        return {
          identity: input.identity,
          method: input.method,
          nativeVersion: owner.catalog.version,
          catalogHash,
          responseSchema: definition.outputSchema,
        } as Agent.InputDefinition;
      },
      "agent.notifications": (args) =>
        notifications.page(args as Agent.NotificationQuery, journal.epoch),
      "agent.projects": async () => {
        if (!projectObserved) await freshProjects(service!.connection);
        return { ...projectSnapshot.projects, defaults: locations!.defaults };
      },
      "agent.answer": (args, context) => {
        const answer = args as Agent.AnswerInput;
        requireThat(
          !context.operationId || context.operationId === answer.operationId,
          "mutation_conflict",
          "Outer and answer operation IDs must match.",
        );
        const key = identity(context, answer.operationId);
        return operations.run(
          key,
          "agent.answer",
          { identity: answer.identity, reply: answer.reply },
          async () => {
            try {
              available();
              const input = journal.input(answer.identity);
              requireThat(
                input,
                "not_found",
                "The exact native input was not observed by this owner.",
              );
              requireThat(
                input.method !== "currentTime/read",
                "native_clock_owned",
                "Native host clock requests are answered by their owning AgentManager.",
              );
              owner.rpc.answer(
                input.method,
                answer.identity.requestId,
                answer.reply,
                () => {
                  journal.dispatchAnswer(key, answer.identity, answer.reply);
                },
              );
            } catch (error) {
              if (journal.get(key)?.phase === "accepted")
                journal.rejectBeforeDispatch(key, IvyError.from(error).code);
              throw error;
            }
            return journal.get(key)!;
          },
        );
      },
    };
    const jsonHandlers: Record<string, JsonToolHandler> = {};
    for (const definition of owner.catalog.clientRequests)
      jsonHandlers["codex." + definition.method] = (args, context) =>
        isReadOnlyNativeMethod(definition.method)
          ? track(executeNativeRead(definition.method, args, context))
          : track(
              executeNative(definition.method, args, context).then(outcome),
            );
    const probe = registry.namespaces[0]!.tools.find(
      (value) => value.name === "account/rateLimits/read",
    );
    requireThat(
      probe,
      "native_catalog_mismatch",
      "Pinned native catalog lacks the null-argument transport probe.",
    );
    service = new ServiceClient({
      publicBaseUrl: config.publicBaseUrl,
      credential: () => config.credential!,
      identity: {
        serviceNodeId: config.serviceNodeId,
        serviceName: "agent-manager",
        hostId: config.hostId,
        version: config.version,
        buildId: config.buildId,
        hiveProtocol: 1,
        nativeVersion: settings.nativeVersion,
      },
      registry: () => registry,
      handlers,
      jsonHandlers,
      heartbeatMs: 2000,
      reconcile: async (connection) => {
        hiveConnection = connection;
        state.setRuntimeEpoch(
          (await connection.request("system.status", {})).runtimeEpoch,
        );
        projectSnapshot = inventory.currentProjects();
        locations!.setNativeProjects(projectSnapshot.projects.projects);
        available();
        // Registry is synchronized but this generation is not ready: no provider invocation is allowed.
        // Older protocol-1 Hive builds reject null at the outer transport and must be upgraded first.
        let compatible = false;
        try {
          await connection.request("tools.call", {
            qualifiedName: "codex." + probe.name,
            serviceNodeId: config.serviceNodeId,
            expectedDefinitionHash: hashJson(probe),
            arguments: null,
          });
        } catch (error) {
          compatible =
            error instanceof IvyError &&
            error.code === "service_not_ready" &&
            error.outcome === "not_executed";
        }
        requireThat(
          compatible,
          "hive_native_json_unsupported",
          "Hive did not verify unchanged native JSON transport before readiness.",
        );
        if (instructions && mcp && skills) {
          await Promise.all([
            instructions.synchronize(connection),
            mcp.synchronize(connection),
            skills.synchronize(connection),
          ]);
          environmentAt = Date.now();
        }
        void refreshThreads(connection).catch(() => undefined);
        void refreshProjects(connection).catch(() => undefined);
      },
      readiness: async (connection) => {
        if (
          instructions &&
          mcp &&
          skills &&
          Date.now() - environmentAt >= 10_000
        ) {
          environmentAt = Date.now();
          void Promise.all([
            instructions.synchronize(connection),
            mcp.synchronize(connection),
            skills.synchronize(connection),
          ]);
        }
        if (!threadRefreshWork && Date.now() - threadRefreshAt >= 60_000)
          void refreshThreads(connection).catch(() => undefined);
        if (!projectRefreshWork && Date.now() - projectRefreshAt >= 300_000)
          void refreshProjects(connection).catch(() => undefined);
        const ready = !closing && owner.rpc.connected;
        const inventoryCode = threadInventoryCode ?? projectInventoryCode,
          now = new Date().toISOString(),
          code = inventoryCode ?? "native_inventory_unavailable";
        await writeHealth(
          ready,
          connection.generation,
          ready
            ? "Native owner available; inventory refresh runs independently."
            : "Native owner is unavailable.",
        );
        return {
          ready,
          diagnostics: [
            {
              code,
              resource: { serviceNodeId: config.serviceNodeId },
              source: config.serviceNodeId,
              severity: ready ? (inventoryCode ? "warning" : "info") : "error",
              status: inventoryCode || !ready ? "current" : "resolved",
              firstObservedAt: now,
              lastObservedAt: now,
              message: inventoryCode
                ? "Native inventory refresh failed; the prior complete snapshot is retained."
                : ready
                  ? "Native owner and inventory are available."
                  : "Native owner or complete inventory is unavailable.",
            },
          ],
        };
      },
      onState: (state) => {
        if (!closing && state.status === "offline")
          process.stderr.write(JSON.stringify({
            at: new Date().toISOString(), event: "agent_hive_connection_failed",
            generation: state.generation ?? null, code: state.code, phase: state.phase,
          }) + "\n");
        if (
          !closing &&
          ["offline", "stopped", "connecting"].includes(state.status)
        )
          void writeHealth(
            false,
            state.generation ?? null,
            "Hive generation is not reconciled" +
              (state.code ? ": " + state.code : "") +
              ".",
          ).catch((error) => {
            // A transient health-file write failure must not destroy the native owner
            // and its running turns. Readiness retries the write; the runtime owner still
            // enforces its existing bounded stale/unready deadline.
            const code =
              (error as NodeJS.ErrnoException).code ??
              IvyError.from(error).code;
            process.stderr.write(
              JSON.stringify({ event: "agent_health_write_failed", code }) +
                "\n",
            );
          });
      },
    });
    timer = setInterval(() => {
      if (controlBusy || closing) return;
      controlBusy = true;
      void health
        .control()
        .then((value) => {
          if (value) return close();
        })
        .catch(() => fail("agent_control_failed"))
        .finally(() => {
          controlBusy = false;
        });
    }, 500);
    service.start();
    return { service, closed, close: () => close() };
  } catch (error) {
    await close("agent_start_failed").catch(() => undefined);
    throw error;
  }
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    const manager = await startAgentManager(configurationPath());
    const stop = () => {
      void manager.close().catch(() => {
        process.exitCode = 1;
      });
    };
    process.once("SIGTERM", stop);
    process.once("SIGINT", stop);
    const code = await manager.closed;
    process.exitCode = code === "agent_stopped" ? 0 : 1;
  } catch (error) {
    const failure = IvyError.from(error), details = failure.details as { method?: unknown; timeoutMs?: unknown } | undefined;
    process.stderr.write(
      JSON.stringify({
        at: new Date().toISOString(),
        code: failure.code,
        message: "AgentManager failed to start.",
        ...(typeof details?.method === "string" ? { method: details.method } : {}),
        ...(typeof details?.timeoutMs === "number" ? { timeoutMs: details.timeoutMs } : {}),
      }) + "\n",
    );
    process.exitCode = 1;
  }
}
