import assert from "node:assert/strict";
import { mkdir, readFile, realpath } from "node:fs/promises";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { randomUUID } from "node:crypto";
import { createServer } from "node:net";
import { once } from "node:events";

// Generate fresh installation settings without importing accounts, credentials, browser profiles
// or prior runtime state. Creating configuration does not install or start any service.
const { values } = parseArgs({
  options: Object.fromEntries(
    ["distribution", "root", "executables", "native-config"].map((key) => [
      key,
      { type: "string" },
    ]),
  ),
});
for (const key of ["distribution", "root", "executables", "native-config"])
  assert.ok(values[key], "Missing --" + key);
const distribution = await realpath(resolve(values.distribution)),
  root = resolve(values.root);
const load = (path) =>
  import(pathToFileURL(join(distribution, "dist", path)).href);
const { inside, atomicJson } = await load(
  "packages/host-runtime/src/config.js",
);
const { privateDirectory } = await load(
  "packages/host-runtime/src/backup-files.js",
);
const { validateHost, validateAgent, validateAutomation, validateTaskBoard } =
  await load("packages/contracts/src/validation.js");
const { hashJson } = await load("packages/contracts/src/canonical.js");
assert.ok(
  inside(
    await realpath(resolve(".local")),
    await realpath(resolve(root, "..")),
  ),
);
await privateDirectory(root);
const id = randomUUID().slice(0, 8),
  hostId = "ivy-installed-" + id + "-acceptance",
  configPath = join(root, "config.json");
const executables = JSON.parse(
  await readFile(resolve(values.executables), "utf8"),
).executables;
assert.ok(executables?.node && executables?.git && executables?.tar);
const native = JSON.parse(
  await readFile(resolve(values["native-config"]), "utf8"),
).instances.find(
  (instance) => instance.componentId === "agent-manager",
)?.settings;
assert.ok(
  native?.nativeExecutable &&
    native?.nativeVersion &&
    native?.nativeExecutableHash,
);
const reservation = createServer();
reservation.listen(0, "127.0.0.1");
await once(reservation, "listening");
const port = reservation.address().port;
await new Promise((yes, no) => {
  reservation.close((error) => (error ? no(error) : yes()));
});
const paths = Object.fromEntries(
  [
    "runtime",
    "artifacts",
    "staging",
    "project",
    "services",
    "codex-projects",
  ].map((name) => [name, join(root, name)]),
);
for (const path of Object.values(paths)) await mkdir(path);
// This public-only role set mirrors the reusable owners in the selected reference
// configuration. Account-, device- and private-integration components are added later
// through their own explicit configuration and are not prerequisites for installation.
const components = [
  "hive",
  "host-executor",
  "service-manager",
  "agent-manager",
  "automation-example",
  "task-board",
];
const principalId = "ivy-owner-" + id,
  token = randomUUID();
const node = (component) => hostId + "." + component;
const instanceId = (component) => component + "-" + id;
const data = (component) => join(paths.services, component, "data");
const settings = {
  hive: {
    listenHost: "127.0.0.1",
    listenPort: port,
    credentials: [{ principalId, token }],
    packagePublisherPrincipalIds: [principalId],
    backup: { directory: join(root, "backups"), intervalHours: 24, retain: 7 },
  },
  "host-executor": { hostConfigPath: configPath },
  "service-manager": { hostConfigPath: configPath },
  "agent-manager": {
    nativeExecutable: native.nativeExecutable,
    nativeVersion: native.nativeVersion,
    nativeExecutableHash: native.nativeExecutableHash,
    ...(native.windowsShell ? { windowsShell: native.windowsShell } : {}),
    codexHome: join(data("agent-manager"), "codex-home"),
    appServer: { mode: "owned-stdio" },
projectRoot: paths.project,
    internalProjectRoot: paths["codex-projects"],
    limits: {
      maxOperations: 100000,
      maxJournalBytes: 536870912,
      maxPendingInputs: 64,
      maxNotificationBytes: 16777216,
    },
  },
  "automation-example": {
    maximumPeriodsPerTick: 3,
    pollMs: 1000,
    definition: {
      scheduleId: "installed-" + id,
      principalId,
      rootObjectId: null,
      timeZone: "UTC",
      firstDate: new Date(Date.now() - 86400000).toISOString().slice(0, 10),
      localTime: "00:00",
      catchUp: "all",
      subscriptionName: "installed-" + id,
      inputSource: "service:" + hostId + ".automation-input-" + id,
      inputTopic: "automation-input.sample",
      initialSequence: 0,
    },
  },
  taskBoard: {
    principalId,
    rootObjectId: null,
    scheduler: { enabled: true, intervalMs: 2000, pageSize: 50 },

    phoneTarget: null,
  },
};
validateHost("HiveSettings", settings.hive);
validateHost("ManagerSettings", settings["service-manager"]);
validateAgent("Settings", settings["agent-manager"]);
validateAutomation("Settings", settings["automation-example"]);
validateTaskBoard("Settings", settings.taskBoard);
const config = {
  schemaVersion: 1,
  hostId,
  ivyRoot: root,
  servicesRoot: paths.services,
  configPath,
  runtimeRoot: paths.runtime,
  artifactRoot: paths.artifacts,
  stagingRoot: paths.staging,
  publicBaseUrl: `http://127.0.0.1:${port}/ivy`,
  executables,
  packageUpdates: { intervalSeconds: 60 },
  instances: components.map((componentId) => ({
    instanceId: instanceId(componentId),
    serviceNodeId: node(componentId),
    componentId,
    enabled: false,
    engine: "process",
    ...(componentId === "service-manager"
      ? { process: { allowWindowsBreakaway: true } }
      : {}),
    credential: token,
    ...(componentId === "hive"
      ? { secretPaths: ["/settings/credentials"] }
      : {}),
    settings: settings[componentId],
  })),
};
validateHost("HostConfig", config);
await atomicJson(configPath, config);
await atomicJson(join(root, "configuration-receipt.json"), {
  schemaVersion: 1,
  hostId,
  configPath,
  configHash: hashJson(config),
  accountCredentialsCopied: false,
  servicesStarted: false,
  components,
  remainingSetup: [
    "Select and verify all component candidates before installation.",
    "Keep optional services disabled until their local accounts, projects and targets have been configured and verified.",
  ],
});
console.log(JSON.stringify({ hostId, configPath, servicesStarted: false }));
