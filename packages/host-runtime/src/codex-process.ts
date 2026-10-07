import { mkdir, realpath, stat } from "node:fs/promises";
import { isAbsolute, join } from "node:path";
import { randomUUID } from "node:crypto";
import { fileHash } from "./artifact.js";
import { jsonFile, inside } from "./config.js";
import { startProcess, runCommand } from "./process.js";
import { resolveCodexHome, expectedServerHome } from "./codex-home.js";
import type { AppServerSettings } from "./codex-home.js";
import {
  ensureSharedDaemon,
  initializedServerVersion,
} from "./codex-daemon.js";
import { proxyTransport } from "./codex-proxy-transport.js";
import { NativeRpc } from "./codex-rpc.js";
import type { NativeRequest, NativeNotification } from "./codex-rpc.js";
import {
  verifyWindowsShell,
  windowsShellEnvironment,
} from "./codex-windows-shell.js";
import {
  requireClearProcessStopFence,
  recordProcessStopFence,
} from "./process-fence.js";
import {
  requireThat,
  validateAgent,
  nativeCatalogPath,
} from "../../sdk/src/node.js";
import type { Agent, Wire } from "../../sdk/src/node.js";

export interface CodexProcessSettings extends AppServerSettings {
  nativeExecutable: string;
  nativeExecutableHash: string;
  nativeVersion: string;
  windowsShell?: Agent.WindowsShell;
}

/** Hidden public app-server transport shared by service clients. Never touches Desktop UI. */
export async function startCodexProcess(options: {
  settings: CodexProcessSettings;
  artifactRoot: string;
  dataRoot: string;
  clientName: string;
  clientVersion: string;
  hiveCredential?: string;
  onRequest: (request: NativeRequest) => void;
  onNotification: (notification: NativeNotification) => void;
  onClose: (code: string) => void;
}) {
  const { settings } = options;
  requireThat(
    settings.appServer?.mode !== "claude-adapter",
    "native_transport_unavailable",
    "Codex Voice requires the Codex app-server.",
  );
  const home = resolveCodexHome(settings).path;
  const external =
    settings.appServer?.mode === "external-proxy"
      ? settings.appServer
      : settings.appServer?.mode === "owned-stdio"
        ? null
        : process.platform === "win32"
          ? { mode: "external-proxy" as const }
          : null;
  requireThat(
    isAbsolute(settings.nativeExecutable) &&
      isAbsolute(options.dataRoot) &&
      isAbsolute(options.artifactRoot) &&
      !inside(options.artifactRoot, home) &&
      !inside(home, options.artifactRoot),
    "invalid_arguments",
    "Codex executable, runtime storage and home must select distinct absolute paths.",
  );
  await requireClearProcessStopFence(options.dataRoot);
  const catalog = await jsonFile<Agent.Catalog>(
    nativeCatalogPath(
      join(options.artifactRoot, "dist"),
      settings.nativeVersion,
    ),
    2 * 1024 * 1024,
  );
  validateAgent("Catalog", catalog);
  requireThat(
    catalog.provider === "codex" &&
      catalog.version === settings.nativeVersion &&
      catalog.nativeExecutableHash === settings.nativeExecutableHash &&
      catalog.clientRequests.some(
        (value) =>
          value.method === "thread/realtime/start" &&
          JSON.stringify(value.inputSchema).includes("initialItems"),
      ),
    "native_build_mismatch",
    "Configured Codex must match its pinned catalog with realtime startup items.",
  );
  const executable = await realpath(settings.nativeExecutable);
  requireThat(
    (await stat(executable)).isFile() &&
      !/\.(cmd|bat|ps1)$/i.test(executable) &&
      (await fileHash(executable)) === settings.nativeExecutableHash,
    "native_build_mismatch",
    "Configured Codex executable differs from its exact catalog build.",
  );
  await mkdir(home, { recursive: true, mode: 0o700 });
  await mkdir(options.dataRoot, { recursive: true, mode: 0o700 });
  const nativeHome = await realpath(home);
  const expectedHome = external
    ? await realpath(expectedServerHome(settings))
    : nativeHome;
  requireThat(
    !external ||
      external.socketPath === undefined ||
      isAbsolute(external.socketPath),
    "invalid_arguments",
    "Codex proxy requires an absolute local control socket.",
  );
  requireThat(
    !external ||
      external.socketPath !== undefined ||
      expectedHome === nativeHome,
    "native_home_mismatch",
    "Automatic shared daemon startup must select the resolved Codex home.",
  );
  const jobLauncher = join(
    options.artifactRoot,
    "dist",
    "native",
    "ivy-job.exe",
  );
  const shell =
    process.platform === "win32" && (!external || !external.socketPath)
      ? await verifyWindowsShell(settings.windowsShell, nativeHome, jobLauncher)
      : null;
  const environment = {
    CODEX_HOME: nativeHome,
    ...(shell ? windowsShellEnvironment(shell) : {}),
    ...(!external && options.hiveCredential
      ? { IVY_HIVE_TOKEN: options.hiveCredential }
      : {}),
  };
  const binaries = { codex: executable };
  const version = await runCommand(
    { executable: "codex", args: ["--version"], timeoutMs: 15000 },
    nativeHome,
    binaries,
    { environment, jobLauncher },
  );
  requireThat(
    version.exitCode === 0 &&
      version.stdout.trim() === "codex-cli " + settings.nativeVersion,
    "native_build_mismatch",
    "Codex CLI version differs from the selected catalog.",
  );
  const daemon =
    external && !external.socketPath
      ? await ensureSharedDaemon({
          executable,
          executableHash: settings.nativeExecutableHash,
          version: settings.nativeVersion,
          home: nativeHome,
          environment,
          jobLauncher,
        })
      : null;
  const socketPath = external
    ? (external.socketPath ?? daemon!.socketPath)
    : null;
  const epoch = randomUUID();
  const child = await startProcess(
    {
      executable: "codex",
      args: external
        ? ["app-server", "proxy", "--sock", socketPath!]
        : ["app-server"],
      timeoutMs: 30000,
    },
    nativeHome,
    binaries,
    { environment, jobLauncher, captureOutput: false },
  );
  let rpc: NativeRpc | undefined;
  let transport: Awaited<ReturnType<typeof proxyTransport>> | undefined;
  const completion = child.completion.then(async (result) => {
    if (result.errorCode === "outcome_unknown")
      await recordProcessStopFence(options.dataRoot, {
        schemaVersion: 1,
        owner: "native",
        ownerId: epoch,
        processId: child.child.pid ?? null,
        observedAt: new Date().toISOString(),
        code: "outcome_unknown",
      });
    rpc?.close(result.errorCode ?? "native_process_exited");
    return result;
  });
  void completion.catch(() => rpc?.close("native_stop_evidence_failed"));
  let closing: Promise<void> | null = null;
  const close = (): Promise<void> =>
    (closing ??= (async () => {
      rpc?.close("native_owner_stopping");
      await transport?.close();
      await child.stop(2000);
      const result = await completion;
      requireThat(
        result.errorCode !== "outcome_unknown",
        "outcome_unknown",
        "Codex process cleanup remains unknown.",
      );
    })());
  try {
    if (external)
      transport = await proxyTransport(child.child.stdin!, child.child.stdout!);
    rpc = new NativeRpc(
      catalog,
      transport?.input ?? child.child.stdin!,
      transport?.output ?? child.child.stdout!,
      {
        onRequest: options.onRequest,
        onNotification: options.onNotification,
        onClose: options.onClose,
      },
    );
    void rpc.closed.then(close).catch(() => undefined);
    const reply = await rpc.request(
      "initialize",
      {
        clientInfo: {
          name: options.clientName,
          version: options.clientVersion,
        },
        capabilities: { experimentalApi: true, requestAttestation: false },
      },
      {},
      30000,
    );
    requireThat(
      "result" in reply,
      "native_initialization_failed",
      "Codex initialization did not succeed.",
    );
    const observed = reply.result as Record<string, Wire.Json>;
    requireThat(
      typeof observed["codexHome"] === "string" &&
        (await realpath(observed["codexHome"])) === expectedHome,
      "native_home_mismatch",
      "Connected app-server belongs to a different Codex home.",
    );
    requireThat(
      initializedServerVersion(observed["userAgent"]) ===
        settings.nativeVersion,
      "native_server_version_mismatch",
      "Connected app-server does not match the pinned protocol version.",
    );
    rpc.notify("initialized");
    return {
      rpc,
      home: expectedHome,
      epoch,
      close,
      completion,
      mode: external ? ("external-proxy" as const) : ("owned-stdio" as const),
    };
  } catch (error) {
    await close();
    throw error;
  }
}
