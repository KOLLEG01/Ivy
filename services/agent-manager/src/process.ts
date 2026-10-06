import { mkdir, realpath, stat } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { dirname, isAbsolute, join, resolve } from "node:path";
import { jsonFile, inside } from "../../../packages/host-runtime/src/config.js";
import {
  runCommand,
  startProcess,
} from "../../../packages/host-runtime/src/process.js";
import { fileHash } from "../../../packages/host-runtime/src/artifact.js";
import {
  recordProcessStopFence,
  requireClearProcessStopFence,
} from "../../../packages/host-runtime/src/process-fence.js";
import { requireThat, IvyError } from "../../../packages/sdk/src/node.js";
import { validateAgent } from "../../../packages/sdk/src/node.js";
import type { Agent, Wire } from "../../../packages/sdk/src/node.js";
import { NativeRpc } from "./rpc.js";
import { proxyTransport } from "./proxy-transport.js";
import type { NativeRequest, NativeNotification } from "./rpc.js";
import {
  verifyWindowsShell,
  windowsShellEnvironment,
} from "./windows-shell.js";
import {
  resolveCodexHome,
  instanceOwnedCodexHome,
  usesExternalAppServer,
  expectedServerHome,
} from "../../../packages/sdk/src/host.js";
import { nativeCatalogPath } from "../../../packages/sdk/src/node.js";
import { ensureSharedDaemon, initializedServerVersion } from "./daemon.js";
import {
  ownedProcessTreeStopped,
  retainOwnedProcessStop,
} from "./process-evidence.js";
import {
  assertClaudeSubscriptionConfig,
  checkedClaudeAdapter,
} from "./claude-adapter.js";

export interface NativeProcessOptions {
  artifactRoot: string;
  dataRoot: string;
  settings: Agent.Settings;
  clientVersion: string;
  /** Explicit credential of this AgentManager instance, never inherited from the desktop process. */
  hiveCredential?: string;
  /** Apply managed Codex-home inputs after path validation and before any native process starts. */
  beforeLaunch?: (nativeHome: string) => Promise<void>;
  beginEpoch: (epoch: string) => void;
  onClose: (epoch: string, code: string) => void;
  onRequest: (epoch: string, value: NativeRequest) => void;
  onNotification: (epoch: string, value: NativeNotification) => void;
}

/** Own the launched stdio server, or only the explicit proxy connection to an external server. */
export async function startNativeProcess(options: NativeProcessOptions) {
  const { settings } = options;
  validateAgent("Settings", settings);
  const home = resolveCodexHome(settings),
    ownedHome = instanceOwnedCodexHome(settings, options.dataRoot);
  const claude =
    settings.appServer?.mode === "claude-adapter" ? settings.appServer : null;
  if (claude)
    requireThat(
      ownedHome !== null &&
        settings.patcherEnabled !== false &&
        settings.skillsRoot === undefined,
      "invalid_arguments",
      "Claude adapter requires an instance-owned home, skills and managed environment.",
    );
  const external: Extract<
    NonNullable<Agent.Settings["appServer"]>,
    { mode: "external-proxy" }
  > | null = usesExternalAppServer(settings)
    ? settings.appServer?.mode === "external-proxy"
      ? settings.appServer
      : { mode: "external-proxy" }
    : null;
  requireThat(
    isAbsolute(options.dataRoot) &&
      isAbsolute(options.artifactRoot) &&
      isAbsolute(settings.nativeExecutable) &&
      !inside(options.artifactRoot, home.path) &&
      !inside(home.path, options.artifactRoot) &&
      home.path !== resolve(options.dataRoot),
    "invalid_arguments",
    "Codex home must not overlap executable artifacts or replace the instance data root.",
  );
  if (external)
    requireThat(
      (external.socketPath === undefined || isAbsolute(external.socketPath)) &&
        isAbsolute(expectedServerHome(settings)),
      "invalid_arguments",
      "An explicit external socket and expected server home must be absolute.",
    );
  await requireClearProcessStopFence(options.dataRoot);
  requireThat(
    /^0\.\d+\.\d+$/.test(settings.nativeVersion),
    "native_catalog_unavailable",
    "Native version must identify an exact generated catalog.",
  );
  const catalog = await jsonFile<Agent.Catalog>(
    nativeCatalogPath(
      join(options.artifactRoot, "dist"),
      settings.nativeVersion,
    ),
    2 * 1024 * 1024,
  );
  validateAgent("Catalog", catalog);
  requireThat(
    catalog.version === settings.nativeVersion &&
      catalog.nativeExecutableHash === settings.nativeExecutableHash,
    "native_build_mismatch",
    "Native settings do not match the generated catalog executable identity.",
  );
  const executable = await realpath(settings.nativeExecutable);
  requireThat(
    (await stat(executable)).isFile() && !/\.(cmd|bat|ps1)$/i.test(executable),
    "native_build_mismatch",
    "Native owner requires the direct executable, not a moving shell wrapper.",
  );
  requireThat(
    (await fileHash(executable)) ===
      (claude?.nodeExecutableHash ?? settings.nativeExecutableHash),
    "native_build_mismatch",
    "Configured native executable bytes differ from their generated schema build.",
  );
  const adapterScript = claude
    ? await checkedClaudeAdapter(
        claude.adapterRoot,
        settings.nativeExecutableHash,
      )
    : null;
  const claudeExecutable = claude
    ? await realpath(claude.claudeExecutable)
    : null;
  if (claude)
    requireThat(
      isAbsolute(claude.claudeExecutable) &&
        claudeExecutable !== null &&
        (await stat(claudeExecutable)).isFile() &&
        (await fileHash(claudeExecutable)) === claude.claudeExecutableHash,
      "native_build_mismatch",
      "Claude CLI differs from the configured executable identity.",
    );
  await mkdir(options.dataRoot, { recursive: true, mode: 0o700 });
  const dataRoot = await realpath(options.dataRoot);
  let ancestor = resolve(home.path);
  while (true) {
    try {
      await stat(ancestor);
      break;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
      const parent = dirname(ancestor);
      requireThat(
        parent !== ancestor,
        "invalid_arguments",
        "Native home has no owned existing ancestor.",
      );
      ancestor = parent;
    }
  }
  if (ownedHome)
    requireThat(
      inside(dataRoot, await realpath(ancestor)),
      "target_conflict",
      "Owned native home traverses an existing link outside this instance data.",
    );
  await mkdir(home.path, { recursive: true, mode: 0o700 });
  const nativeHome = await realpath(home.path);
  if (ownedHome)
    requireThat(
      inside(dataRoot, nativeHome),
      "target_conflict",
      "Owned native home resolves outside this instance data.",
    );
  const claudeConfig = claude ? join(dataRoot, "claude-config") : null;
  const adapterHome = claude ? join(dataRoot, "claude-adapter") : null;
  if (claudeConfig && adapterHome) {
    await mkdir(claudeConfig, { recursive: true, mode: 0o700 });
    await mkdir(adapterHome, { recursive: true, mode: 0o700 });
    requireThat(
      inside(dataRoot, await realpath(claudeConfig)) &&
        inside(dataRoot, await realpath(adapterHome)),
      "target_conflict",
      "Claude runtime data must remain inside this instance.",
    );
    await assertClaudeSubscriptionConfig(claudeConfig);
  }
  await options.beforeLaunch?.(nativeHome);
  // Scope CODEX_HOME only to the new child; the current host/task environment is never changed.
  const executables = { codex: executable },
    jobLauncher = join(options.artifactRoot, "dist", "native", "ivy-job.exe");
  requireThat(
    process.platform === "win32" || settings.windowsShell === undefined,
    "native_shell_mismatch",
    "Windows shell configuration belongs only to the Windows native owner.",
  );
  const windowsShell =
    process.platform === "win32" && (!external || !external.socketPath)
      ? await verifyWindowsShell(settings.windowsShell, nativeHome, jobLauncher)
      : null;
  const environment = {
    CODEX_HOME: nativeHome,
    ...(windowsShell ? windowsShellEnvironment(windowsShell) : {}),
    ...(!external && !claude && options.hiveCredential
      ? { IVY_HIVE_TOKEN: options.hiveCredential }
      : {}),
    ...(claude && claudeConfig && adapterHome && claudeExecutable
      ? {
          CLAUDE_CONFIG_DIR: claudeConfig,
          CLAUDE_CODEX_HOME: adapterHome,
          CLAUDE_CODEX_CLI: claudeExecutable,
          CLAUDE_CODEX_RUNTIME_TYPE: "agent-sdk-sidecar",
          CLAUDE_CODEX_PROVIDER: "claude-code",
          CLAUDE_CODEX_AGENT_LOOP: "native-claude-code-sdk",
          CLAUDE_CODEX_DISABLE_CODEX_PROXY: "1",
          CLAUDE_CODEX_SUBSCRIPTION_ONLY: "1",
          CLAUDE_CODEX_DEFAULT_MODEL: claude.defaultModel,
          CLAUDE_CODEX_COMPAT_VERSION: settings.nativeVersion,
          CLAUDE_CODEX_MCP_SERVERS: join(dataRoot, "claude-mcp.json"),
          CLAUDE_CODEX_INSTRUCTIONS_FILE: join(
            nativeHome,
            "AGENTS.override.md",
          ),
        }
      : {}),
  };
  const version = await runCommand(
    {
      executable: "codex",
      args: claude ? [adapterScript!, "app-server", "--help"] : ["--version"],
      timeoutMs: 15_000,
    },
    nativeHome,
    executables,
    { environment, jobLauncher },
  );
  requireThat(
    claude
      ? /claude-codex-adapter app-server/.test(version.stdout)
      : version.stdout.trim() === "codex-cli " + settings.nativeVersion,
    "native_build_mismatch",
    "Actual public native CLI version differs from its pinned schema catalog.",
  );
  if (external) {
    const help = await runCommand(
      {
        executable: "codex",
        args: ["app-server", "proxy", "--help"],
        timeoutMs: 15_000,
      },
      nativeHome,
      executables,
      { environment, jobLauncher },
    );
    requireThat(
      help.exitCode === 0 && /--sock\b/.test(help.stdout),
      "native_transport_unavailable",
      "Installed Codex does not expose the explicit app-server proxy socket transport.",
    );
  }
  const expectedHome = external
    ? await realpath(expectedServerHome(settings)).catch(() => {
        throw new IvyError(
          "native_home_mismatch",
          "The selected external server home is unavailable.",
        );
      })
    : nativeHome;
  requireThat(
    !external ||
      external.socketPath !== undefined ||
      expectedHome === nativeHome,
    "native_home_mismatch",
    "Automatic daemon startup must use the resolved Codex home. A different server home requires an explicit endpoint.",
  );
  const daemon =
    external && !external.socketPath
      ? await ensureSharedDaemon({
          executable,
          executableHash: settings.nativeExecutableHash,
          version: settings.nativeVersion,
          home: nativeHome,
          jobLauncher,
          environment,
        })
      : null;
  const socketPath = external
    ? (external.socketPath ?? daemon!.socketPath)
    : null;
  const epoch = randomUUID();
  options.beginEpoch(epoch);
  const nativeChild = await startProcess(
    {
      executable: "codex",
      args: claude
        ? [adapterScript!, "app-server", "--listen", "stdio://"]
        : external
          ? ["app-server", "proxy", "--sock", socketPath!]
          : ["app-server"],
      timeoutMs: 30_000,
    },
    nativeHome,
    executables,
    { environment, jobLauncher, captureOutput: false },
  ).catch((error) => {
    options.onClose(epoch, "native_launch_failed");
    throw error;
  });
  let rpc: NativeRpc;
  let transport: Awaited<ReturnType<typeof proxyTransport>> | undefined;
  const completion = nativeChild.completion.then(async (result) => {
    if (result.errorCode === "outcome_unknown")
      await recordProcessStopFence(options.dataRoot, {
        schemaVersion: 1,
        owner: "native",
        ownerId: epoch,
        processId: nativeChild.child.pid ?? null,
        observedAt: new Date().toISOString(),
        code: "outcome_unknown",
      });
    else if (
      !external &&
      (await ownedProcessTreeStopped(nativeChild.child.pid))
    )
      await retainOwnedProcessStop(options.dataRoot, epoch);
    rpc?.close(result.errorCode ?? "native_process_exited");
    return result;
  });
  void completion.catch(() => rpc?.close("native_stop_evidence_failed"));
  try {
    if (external)
      transport = await proxyTransport(
        nativeChild.child.stdin!,
        nativeChild.child.stdout!,
      );
    rpc = new NativeRpc(
      catalog,
      transport?.input ?? nativeChild.child.stdin!,
      transport?.output ?? nativeChild.child.stdout!,
      {
        onClose: (code) => options.onClose(epoch, code),
        onRequest: (value) => options.onRequest(epoch, value),
        onNotification: (value) => options.onNotification(epoch, value),
      },
    );
  } catch (error) {
    await nativeChild.stop();
    await completion;
    options.onClose(epoch, "native_protocol_setup_failed");
    throw error;
  }
  let stopping: Promise<void> | null = null;
  const close = (): Promise<void> => {
    if (!stopping) {
      rpc.close("native_owner_stopping");
      stopping = (async () => {
        await transport?.close();
        await nativeChild.stop(2000);
        const result = await completion;
        if (result.errorCode === "outcome_unknown")
          throw new IvyError(
            "outcome_unknown",
            "Native process stop remains unknown; its durable fence prevents another owner.",
            "unknown",
          );
      })();
    }
    return stopping;
  };
  // Protocol failure/timeout closes the owned process tree, even while the owner remains alive.
  void rpc.closed.then(() => close()).catch(() => undefined);
  try {
    const initialized = await rpc.request(
      "initialize",
      {
        clientInfo: {
          name: "ivy-agent-manager",
          title: "Ivy AgentManager",
          version: options.clientVersion,
        },
        capabilities: { experimentalApi: true, requestAttestation: false },
      },
      {},
      30_000,
    );
    requireThat(
      "result" in initialized,
      "native_initialization_failed",
      "Public native initialization returned an error.",
    );
    const observed = initialized.result as Record<string, Wire.Json>;
    requireThat(
      observed &&
        typeof observed === "object" &&
        !Array.isArray(observed) &&
        typeof observed["codexHome"] === "string" &&
        (await realpath(observed["codexHome"])) === expectedHome,
      "native_home_mismatch",
      "Initialized server home differs from its explicit expected assignment.",
    );
    const actualServerVersion =
      external || claude
        ? initializedServerVersion(observed["userAgent"])
        : settings.nativeVersion;
    requireThat(
      actualServerVersion === settings.nativeVersion &&
        (!claude || String(observed["userAgent"]).includes("claude-codex")) &&
        (!daemon || daemon.appServerVersion === actualServerVersion),
      "native_server_version_mismatch",
      "The connected external server version differs from the checked protocol catalog.",
    );
    rpc.notify("initialized");
    return {
      rpc,
      epoch,
      catalog,
      initialized: initialized.result,
      nativeHome: await realpath(String(observed["codexHome"])),
      connectionMode: external
        ? ("external-proxy" as const)
        : ("owned-stdio" as const),
      homeSource: home.source,
      actualServerVersion,
      socketPath,
      daemon,
      ownsNativeHome: !external && ownedHome !== null,
      executable,
      windowsShell,
      launcherPid: nativeChild.child.pid ?? null,
      completion,
      close,
    };
  } catch (error) {
    await close();
    throw error instanceof IvyError
      ? error
      : new IvyError("native_start_failed", "Public native startup failed.");
  }
}
