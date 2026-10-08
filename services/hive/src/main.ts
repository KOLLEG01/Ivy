import { join, resolve, isAbsolute, dirname, basename } from "node:path";
import { fileURLToPath } from "node:url";
import { watch } from "node:fs";
import type { FSWatcher } from "node:fs";
import {
  cp,
  mkdir,
  readFile,
  readdir,
  lstat,
  stat,
  rename,
  rm,
  unlink,
  statfs,
} from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { HiveServer } from "./server.js";
import {
  digest,
  canonical,
} from "../../../packages/contracts/src/canonical.js";
import {
  IvyError,
  requireThat,
} from "../../../packages/contracts/src/errors.js";
import { validateHost } from "../../../packages/contracts/src/host-validation.js";
import type { Host } from "../../../packages/contracts/src/generated.js";
import {
  atomicJson,
  jsonFile,
  configurationPath,
  instanceConfig,
  inside,
} from "../../../packages/host-runtime/src/config.js";
import { HealthFile } from "../../../packages/host-runtime/src/health.js";
import { hiveResetBootstrapMarker } from "../../../packages/host-runtime/src/runtime-maintenance.js";

const log = (code: string, message: string) =>
  process.stderr.write(
    JSON.stringify({ at: new Date().toISOString(), code, message }) + "\n",
  );
export async function collectBackupArtifacts(root: string, now = Date.now(), enabled = true): Promise<void> {
  const directory = resolve(root);
  const entries = await readdir(directory, { withFileTypes: true }).catch(error => {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return []; throw error;
  });
  const present = new Set(entries.map(entry => entry.name));
  const cutoff = now - 24 * 60 * 60 * 1000;
  const prefix = /^hive-\d{4}-\d{2}-\d{2}T[\d.-]+Z-[0-9a-f-]{36}\./;
  for (const entry of entries) {
    if (!prefix.test(entry.name) || !/\.(?:partial|ui\.partial|ui|sqlite(?:\.json|-wal|-shm)?)$/.test(entry.name)) continue;
    const target = resolve(directory, entry.name);
    requireThat(inside(directory, target), 'invalid_arguments', 'Invalid backup cleanup path.');
    if (enabled && (await lstat(target)).mtimeMs > cutoff) continue;
    const base = entry.name.replace(/\.(?:ui\.partial|sqlite(?:\.json|-wal|-shm)?|partial|ui)$/, '');
    if (enabled) {
      if (entry.name.endsWith('.ui') && present.has(base + '.sqlite') && present.has(base + '.sqlite.json')) continue;
      if (entry.name.endsWith('.sqlite') && present.has(base + '.sqlite.json')) continue;
      if (entry.name.endsWith('.sqlite.json') && present.has(base + '.sqlite')) continue;
      if (/\.sqlite-(?:wal|shm)$/.test(entry.name) && present.has(base + '.sqlite')) continue;
    }
    if (entry.isFile() || entry.isDirectory()) await rm(target, { recursive: entry.isDirectory(), force: true });
  }
}
function settingsOf(config: Host.InstanceConfig): Host.HiveSettings {
  requireThat(
    config.componentId === "hive",
    "invalid_arguments",
    "Hive requires its own instance configuration.",
  );
  const settings = config.settings as unknown as Host.HiveSettings;
  validateHost("HiveSettings", settings);
  requireThat(
    settings.credentials.length > 0 &&
      new Set(settings.credentials.map((credential) => credential.token))
        .size === settings.credentials.length,
    "invalid_arguments",
    "Hive needs distinct configured credentials.",
  );
  requireThat(
    isAbsolute(settings.backup.directory) &&
      !inside(config.artifactRoot, settings.backup.directory),
    "invalid_arguments",
    "Backup storage must be outside immutable artifacts.",
  );
  return settings;
}
async function resetBootstrapCredential(
  dataRoot: string,
): Promise<string | undefined> {
  let value: unknown;
  try {
    value = JSON.parse(
      await readFile(hiveResetBootstrapMarker(dataRoot), "utf8"),
    );
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined;
    throw error;
  }
  const marker = value as {
    phase?: unknown;
    publisherCredentialDigest?: unknown;
  } | null;
  if (marker?.phase === "released") return undefined;
  requireThat(
    marker?.phase === "bootstrap",
    "storage_invalid",
    "Runtime reset bootstrap phase is invalid.",
  );
  const digest = marker.publisherCredentialDigest;
  requireThat(
    typeof digest === "string" && /^sha256:[0-9a-f]{64}$/.test(digest),
    "storage_invalid",
    "Runtime reset bootstrap credential is invalid.",
  );
  return digest;
}
export async function startHive(path: string): Promise<{
  server: HiveServer;
  close: () => Promise<void>;
  backup: () => Promise<string>;
}> {
  let config = await instanceConfig(path),
    settings = settingsOf(config);
  const resetBootstrapCredentialDigest = await resetBootstrapCredential(
    config.dataRoot,
  );
  if (settings.backup.enabled !== false) await mkdir(settings.backup.directory, { recursive: true });
  await collectBackupArtifacts(settings.backup.directory, Date.now(), settings.backup.enabled !== false);
  const backupEntries = new Set(await readdir(settings.backup.directory).catch(error => {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return []; throw error;
  }));
  const snapshots = [...backupEntries].filter(
    (file) => file.startsWith("hive-") && file.endsWith(".sqlite") &&
      backupEntries.has(file + '.json') && backupEntries.has(file.replace(/\.sqlite$/, '.ui')),
  );
  const newest = snapshots.length
    ? Math.max(
        ...(await Promise.all(
          snapshots.map((file) =>
            stat(join(settings.backup.directory, file)).then(
              (value) => value.mtimeMs,
            ),
          ),
        )),
      )
    : 0;
  const health = new HealthFile(config);
  const credentials = (value: Host.HiveSettings) =>
    value.credentials.map((credential) => ({
      principalId: credential.principalId,
      digest: digest(credential.token),
    }));
  const server = new HiveServer({
    filename: join(config.dataRoot, "hive.sqlite"),
    publicBaseUrl: config.publicBaseUrl,
    version: config.version,
    buildId: config.buildId,
    credentials: credentials(settings),
    listenHost: settings.listenHost,
    listenPort: settings.listenPort,
    validateMessages: settings.debug?.validateMessages === true,
    packageRoot: join(config.dataRoot, "packages"),
    uiRoot: join(config.dataRoot, "ui-releases"),
    packagePublisherPrincipalIds: settings.packagePublisherPrincipalIds ?? [],
    observeMcpCall: (observation) =>
      process.stderr.write(
        JSON.stringify({
          at: new Date().toISOString(),
          event: "mcp_call",
          ...observation,
        }) + "\n",
      ),
    consoleRoot: join(config.artifactRoot, "dist", "console"),
    ...(settings.trustedProxyAddresses
      ? { trustedProxyAddresses: settings.trustedProxyAddresses }
      : {}),
    ...(resetBootstrapCredentialDigest
      ? { resetBootstrapCredentialDigest }
      : {}),
  });
  await health.write(false, "Starting storage and HTTP listener.");
  try {
    await server.start();
  } catch (error) {
    await health.write(false, "Hive startup failed.");
    throw error;
  }
  let stopped = false,
    timer: NodeJS.Timeout | null = null,
    controlTimer: NodeJS.Timeout | null = null,
    controlBusy = false,
    backupTask: Promise<string> | null = null;
  let configWatcher: FSWatcher | null = null,
    reloadTimer: NodeJS.Timeout | null = null;
  let nextBackupAt = 0,
    nextRetentionAt = 0,
    lastConfig = canonical(config),
    runningTick: Promise<void> | null = null;
  const lastDiagnostic = new Map<string, { state: string; sentAt: number }>();
  const diagnostic = async (
    code: string,
    message: string,
    resolved = false,
  ) => {
    const state = `${resolved}:${message}`;
    const previous = lastDiagnostic.get(code);
    if (previous?.state === state && Date.now() - previous.sentAt < 60_000)
      return;
    const now = new Date().toISOString();
    await server.worker
      .request({
        action: "diagnostic",
        value: {
          code,
          message,
          resource: { instanceId: config.instanceId },
          source: "hive",
          severity: resolved ? "info" : "error",
          status: resolved ? "resolved" : "current",
          firstObservedAt: now,
          lastObservedAt: now,
        },
      })
      .then(() => {
        lastDiagnostic.set(code, { state, sentAt: Date.now() });
      })
      .catch(() => undefined);
  };
  await diagnostic(
    "configuration_reload_failed",
    "Configuration loaded successfully.",
    true,
  );
  const backup = (): Promise<string> => {
    requireThat(settings.backup.enabled !== false, 'backup_disabled', 'Local Hive backups are disabled by installation configuration.');
    if (backupTask) return backupTask;
    backupTask = (async () => {
      const root = resolve(settings.backup.directory);
      await mkdir(root, { recursive: true });
      await collectBackupArtifacts(root);
      const name =
        "hive-" +
        new Date().toISOString().replaceAll(":", "-") +
        "-" +
        randomUUID();
      const temporary = join(root, name + ".partial"),
        destination = join(root, name + ".sqlite"),
        uiTemporary = join(root, name + ".ui.partial"),
        uiDestination = join(root, name + ".ui");
      requireThat(inside(root, uiTemporary) && inside(root, uiDestination),
        "invalid_arguments", "Invalid UI backup directory.");
      try {
        const before = await server.worker.request({ action: "ui.pointers" });
        const uiRoot = join(config.dataRoot, "ui-releases");
        await cp(uiRoot, uiTemporary, { recursive: true,
          filter: source => source !== join(uiRoot, '.incoming') && source !== join(uiRoot, '.staging') });
        const result = await server.worker.request<{
          pages: number;
          bytesHash: string;
        }>({ action: "backup", destination: temporary }, 600_000);
        const after = await server.worker.request({ action: "ui.pointers" });
        requireThat(canonical(before) === canonical(after), "configuration_changed",
          "UI releases changed during the Hive backup.");
        await rename(uiTemporary, uiDestination);
        await rename(temporary, destination);
        await atomicJson(destination + ".json", {
          schemaVersion: 1,
          buildId: config.buildId,
          createdAt: new Date().toISOString(),
          uiFilesDirectory: basename(uiDestination),
          ...result,
        });
      } catch (error) {
        await rm(uiTemporary, { recursive: true, force: true });
        await rm(uiDestination, { recursive: true, force: true });
        await rm(temporary, { force: true });
        await rm(destination, { force: true });
        throw error;
      }
      const backupFiles = new Set(await readdir(root));
      const retained = [...backupFiles]
        .filter((file) =>
          /^hive-\d{4}-\d{2}-\d{2}T[\d.-]+Z-[0-9a-f-]+\.sqlite$/.test(file) &&
            backupFiles.has(file + '.json'),
        )
        .sort()
        .reverse();
      for (const file of retained.slice(settings.backup.retain)) {
        const target = resolve(root, file);
        requireThat(
          inside(root, target),
          "invalid_arguments",
          "Invalid backup retention path.",
        );
        await unlink(target);
        await unlink(target + ".json").catch(() => undefined);
        const uiTarget = resolve(root, file.replace(/\.sqlite$/, ".ui"));
        requireThat(inside(root, uiTarget), "invalid_arguments", "Invalid UI backup retention path.");
        await rm(uiTarget, { recursive: true, force: true });
      }
      nextBackupAt = Date.now() + settings.backup.intervalHours * 3_600_000;
      await diagnostic(
        "backup_failed",
        "Latest SQLite-safe backup completed.",
        true,
      );
      return destination;
    })()
      .catch(async (error) => {
        nextBackupAt = Date.now() + 300_000;
        log(
          "backup_failed",
          "SQLite-safe snapshot failed; previous snapshots are retained.",
        );
        await diagnostic(
          "backup_failed",
          "SQLite-safe snapshot failed; inspect host storage and backup records.",
        );
        throw error;
      })
      .finally(() => {
        backupTask = null;
      });
    return backupTask;
  };
  const reload = async () => {
    try {
      const next = await jsonFile<Host.InstanceConfig>(path),
        encoded = canonical(next);
      if (encoded === lastConfig) return;
      validateHost("InstanceConfig", next);
      const nextSettings = settingsOf(next);
      const fixed = (value: Host.InstanceConfig, settings: Host.HiveSettings) =>
        canonical({ ...value, settings: { ...settings, credentials: [] } });
      requireThat(
        fixed(next, nextSettings) === fixed(config, settings),
        "restart_required",
        "Only Hive credentials reload without a deployment restart.",
      );
      await server.setCredentials(credentials(nextSettings));
      settings = nextSettings;
      config = next;
      lastConfig = encoded;
      await diagnostic(
        "configuration_reload_failed",
        "Configuration credentials reloaded.",
        true,
      );
    } catch {
      await diagnostic(
        "configuration_reload_failed",
        "Configuration reload was refused; the last valid configuration remains active.",
      );
    }
  };
  const tick = async () => {
    try {
      const observed = await server.worker
        .request<{ ready: boolean }>({ action: "health" }, 2000, false)
        .catch(() => ({ ready: false }));
      await health.write(
        observed.ready,
        observed.ready ? "" : "Storage is recovering.",
      );
      const disk = await statfs(config.dataRoot);
      if (
        disk.bavail * disk.bsize <
        Math.max(512 * 1024 * 1024, disk.blocks * disk.bsize * 0.05)
      )
        await diagnostic(
          "storage_pressure",
          "Available disk space is below the operating reserve.",
        );
      else
        await diagnostic(
          "storage_pressure",
          "Storage operating reserve is available.",
          true,
        );
      if (observed.ready && settings.backup.enabled !== false && !backupTask && Date.now() >= nextBackupAt)
        void backup().catch(() => undefined);
      if (observed.ready && Date.now() >= nextRetentionAt) {
        nextRetentionAt = Date.now() + 15 * 60_000;
        try {
          await server.worker.request({ action: "retention.collect" }, 30_000);
          await server.collectPackageArtifacts();
          if (!backupTask) await collectBackupArtifacts(settings.backup.directory, Date.now(), settings.backup.enabled !== false);
          await diagnostic(
            "retention_failed",
            "Automatic retention collection completed.",
            true,
          );
        } catch {
          await diagnostic(
            "retention_failed",
            "Automatic retention collection failed.",
          );
        }
      }
    } catch {
      log(
        "health_observation_failed",
        "Hive health/configuration observation failed.",
      );
    } finally {
      if (!stopped)
        timer = setTimeout(() => {
          runningTick = tick();
        }, 2000);
    }
  };
  // Existing complete snapshot timestamps survive restarts; restarting is not a backup loop.
  nextBackupAt = newest + settings.backup.intervalHours * 3_600_000;
  configWatcher = watch(dirname(path), (_event, name) => {
    if (stopped || (name && String(name) !== basename(path))) return;
    if (reloadTimer) clearTimeout(reloadTimer);
    reloadTimer = setTimeout(() => {
      reloadTimer = null;
      void reload();
    }, 100);
  });
  configWatcher.on("error", () => {
    void diagnostic(
      "configuration_reload_failed",
      "Configuration watcher failed; restart Hive to load new credentials.",
    );
  });
  runningTick = tick();
  const close = async () => {
    if (stopped) return;
    stopped = true;
    if (timer) clearTimeout(timer);
    if (reloadTimer) clearTimeout(reloadTimer);
    configWatcher?.close();
    if (controlTimer) clearInterval(controlTimer);
    await runningTick;
    await health.write(false, "Hive is shutting down.").catch(() => undefined);
    await server.close();
  };
  controlTimer = setInterval(() => {
    if (stopped || controlBusy) return;
    controlBusy = true;
    void health
      .control()
      .then(async (control) => {
        if (control?.action === "shutdown" || control?.action === "drain")
          await close();
      })
      .catch(() =>
        log("control_failed", "Runtime shutdown control could not be read."),
      )
      .finally(() => {
        controlBusy = false;
      });
  }, 500);
  return { server, close, backup };
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    const hive = await startHive(configurationPath());
    log("hive_started", "Hive listener and storage started.");
    let shuttingDown = false;
    const shutdown = () => {
      if (shuttingDown) return;
      shuttingDown = true;
      const deadline = setTimeout(() => process.exit(1), 15_000);
      void hive.close().then(
        () => {
          clearTimeout(deadline);
          process.exitCode = 0;
        },
        () => {
          clearTimeout(deadline);
          process.exitCode = 1;
        },
      );
    };
    process.on("SIGINT", shutdown);
    process.on("SIGTERM", shutdown);
  } catch (error) {
    log(
      IvyError.from(error).code,
      "Hive failed to start. Inspect the prepared artifact and instance configuration.",
    );
    process.exitCode = 1;
  }
}
