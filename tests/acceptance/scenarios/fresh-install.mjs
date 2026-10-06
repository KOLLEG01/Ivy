import assert from "node:assert/strict";
import {
  readFile,
  writeFile,
  mkdir,
  readdir,
  realpath,
} from "node:fs/promises";
import { resolve, join } from "node:path";
import { pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { randomUUID } from "node:crypto";
import { spawn } from "node:child_process";
import { setTimeout as delay } from "node:timers/promises";

// Connect ordinary verified candidates to one fresh installation. No build/check substitutions,
// existing-installation adoption, automatic retry, or process-name cleanup are allowed here.
const { values } = parseArgs({
  options: {
    ...Object.fromEntries(
      [
        "distribution",
        "config",
        "installation-root",
        "candidates",
        "evidence",
      ].map((key) => [key, { type: "string" }]),
    ),
    execute: { type: "boolean" },
    "defer-automation": { type: "boolean" },
  },
});
for (const key of [
  "distribution",
  "config",
  "installation-root",
  "candidates",
  "evidence",
])
  assert.ok(values[key], "Missing --" + key);
const distribution = await realpath(resolve(values.distribution));
const load = (path) =>
  import(pathToFileURL(join(distribution, "dist", path)).href);
const { inside, atomicJson } = await load(
  "packages/host-runtime/src/config.js",
);
const { hostConfig } = await load("packages/host-runtime/src/host-config.js");
const { hashJson } = await load("packages/contracts/src/canonical.js");
const { validateHost } = await load("packages/contracts/src/validation.js");
const root = await realpath(resolve(values["installation-root"])),
  configPath = await realpath(resolve(values.config));
assert.ok(inside(await realpath(resolve(".local")), root));
assert.ok(inside(root, configPath));
const selected = JSON.parse(await readFile(configPath, "utf8"));
validateHost("HostConfig", selected);
assert.ok(selected.hostId.endsWith("-acceptance"));
assert.equal(new URL(selected.publicBaseUrl).hostname, "127.0.0.1");
for (const path of [
  selected.runtimeRoot,
  selected.artifactRoot,
  selected.stagingRoot,
]) {
  assert.ok(inside(root, await realpath(path)));
  assert.deepEqual(
    await readdir(path),
    [],
    "Fresh installation roots must be empty.",
  );
}
assert.ok(
  selected.instances.length > 0 &&
    selected.instances.every(
      (instance) => !instance.enabled && instance.engine === "process",
    ),
);
const config = await hostConfig(configPath),
  configHash = hashJson(config);
const entries = JSON.parse(await readFile(resolve(values.candidates), "utf8"));
assert.ok(Array.isArray(entries));
const candidates = await Promise.all(
  entries.map(async (entry) => {
    const candidate = JSON.parse(
      await readFile(resolve(entry.candidate), "utf8"),
    );
    validateHost("Candidate", candidate);
    assert.equal(candidate.candidateId, entry.candidateId);
    assert.match(entry.archiveHash, /^sha256:[a-f0-9]{64}$/);
    return { entry, candidate };
  }),
);
const components = [
  ...new Set(config.instances.map((instance) => instance.componentId)),
].sort();
assert.deepEqual(
  candidates.map((row) => row.candidate.componentId).sort(),
  components,
);
assert.equal(
  config.instances.filter(
    (instance) => instance.componentId === "host-executor",
  ).length,
  1,
);
assert.equal(
  config.instances.filter((instance) => instance.componentId === "hive").length,
  1,
);
const hiveSettings = config.instances.find(
  (instance) => instance.componentId === "hive",
).settings;
validateHost("HiveSettings", hiveSettings);
assert.equal(hiveSettings.listenHost, "127.0.0.1");
assert.equal(new URL(config.publicBaseUrl).protocol, "http:");
assert.equal(
  Number(new URL(config.publicBaseUrl).port || 80),
  hiveSettings.listenPort,
);
assert.ok(inside(root, resolve(hiveSettings.backup.directory)));
assert.equal(
  resolve(
    config.instances.find(
      (instance) => instance.componentId === "host-executor",
    ).settings.hostConfigPath,
  ),
  configPath,
);
const evidence = resolve(values.evidence);
assert.ok(inside(root, evidence));
assert.ok(
  [config.runtimeRoot, config.artifactRoot, config.stagingRoot].every(
    (path) => !inside(path, evidence) && !inside(evidence, path),
  ),
);
await mkdir(evidence);
const reportPath = join(evidence, "report.json");
const report = {
  schemaVersion: 1,
  id: randomUUID(),
  hostId: config.hostId,
  configHash,
  execute: Boolean(values.execute),
  phase: "preflight",
  operations: [],
  candidates: [],
  startedAt: new Date().toISOString(),
};
if (values["defer-automation"])
  assert.ok(components.includes("automation-example"));
const save = async (phase) => {
  report.phase = phase;
  await atomicJson(reportPath, report);
  console.log(JSON.stringify({ phase, reportPath }));
};
await writeFile(reportPath, "{}\n", { flag: "wx" });
await save("preflight_passed");
let temporaryExecutor;
try {
  for (const { entry, candidate } of candidates) {
    const transferReport = join(
      evidence,
      "receive-" + candidate.componentId + ".json",
    );
    await save("receiving-" + candidate.componentId);
    const code = await new Promise((yes, no) => {
      const child = spawn(
        process.execPath,
        [
          join(distribution, "tests/acceptance/scenarios/receive-prepared.mjs"),
          "--config",
          configPath,
          "--candidate",
          resolve(entry.candidate),
          "--incoming",
          resolve(entry.incoming),
          "--expected-id",
          candidate.candidateId,
          "--expected-archive",
          entry.archiveHash,
          "--report",
          transferReport,
        ],
        { cwd: distribution, stdio: "inherit", windowsHide: true },
      );
      child.once("error", no);
      child.once("exit", yes);
    });
    assert.equal(
      code,
      0,
      "Verified candidate transfer failed; retain this installation for inspection.",
    );
    report.candidates.push(
      JSON.parse(await readFile(transferReport, "utf8")).candidate,
    );
  }
  const { bootstrapPlan, installBootstrap } = await load(
    "packages/host-runtime/src/bootstrap.js",
  );
  const executor = report.candidates.find(
    (candidate) => candidate.componentId === "host-executor",
  );
  report.bootstrap = await bootstrapPlan(configPath, executor.candidateId);
  await save("bootstrap_planned");
  if (values["defer-automation"]) {
    const candidate = report.candidates.find(
      (candidate) => candidate.componentId === "automation-example",
    );
    report.deferredAutomation = {
      candidateId: candidate.candidateId,
      instanceIds: config.instances
        .filter((instance) => instance.componentId === "automation-example")
        .map((instance) => instance.instanceId),
      candidateTransfer: join(evidence, "receive-automation-example.json"),
      state: "prepared_not_installed",
    };
  }
  assert.equal(hashJson(await hostConfig(configPath)), configHash);
  if (values.execute) {
    await save("bootstrap_install_intent");
    report.installation = await installBootstrap(
      report.bootstrap,
      executor.artifactRoot,
    );
    assert.equal(report.installation.ok, true);
    await save("bootstrap_installed");
    const { HostExecutor } = await load(
      "packages/host-runtime/src/executor.js",
    );
    temporaryExecutor = new HostExecutor(
      config,
      configPath,
      executor.artifactRoot,
    );
    temporaryExecutor.start();
    let executorFailure;
    void temporaryExecutor.completion
      .then(() => temporaryExecutor.close())
      .catch((error) => {
        executorFailure = error;
      });
    const { cli } = await load("packages/cli/src/main.js");
    const hive = config.instances.find(
      (instance) => instance.componentId === "hive",
    );
    const owner = config.instances.find(
      (instance) => instance.componentId === "host-executor",
    );
    const manager = config.instances.find(
      (instance) => instance.componentId === "service-manager",
    );
    assert.ok(hive && owner && manager);
    const actions = [
      { instance: hive, action: "deploy" },
      { instance: hive, action: "enable" },
      ...config.instances
        .filter(
          (instance) =>
            instance !== hive &&
            !(
              values["defer-automation"] &&
              instance.componentId === "automation-example"
            ),
        )
        .map((instance) => ({ instance, action: "deploy" })),
      { instance: manager, action: "enable" },
      { instance: manager, action: "restart" },
      { instance: owner, action: "enable" },
    ];
    for (const { instance, action } of actions) {
      const candidate = report.candidates.find(
        (candidate) => candidate.componentId === instance.componentId,
      );
      const operation = {
        operationId:
          report.id +
          "-" +
          action +
          "-" +
          hashJson(instance.instanceId).slice(7, 31),
        action,
        instanceId: instance.instanceId,
        candidateId: candidate.candidateId,
      };
      report.operations.push(operation);
      await save("deploy_intent");
      const accepted = await cli([
        action,
        "--config",
        configPath,
        "--instance",
        instance.instanceId,
        ...(action === "deploy" ? ["--candidate", candidate.candidateId] : []),
        "--operation-id",
        operation.operationId,
        "--json",
      ]);
      operation.acceptance = accepted.output;
      await save("deploy_accepted");
      assert.equal(accepted.exitCode, 0);
      assert.ok(accepted.output.deploymentId);
      const deadline = Date.now() + 630000;
      while (Date.now() < deadline) {
        if (executorFailure) throw executorFailure;
        const current = await cli([
          "status",
          "--config",
          configPath,
          "--deployment",
          accepted.output.deploymentId,
          "--json",
        ]);
        if (
          ["succeeded", "failed", "needs_attention", "rolled_back"].includes(
            current.output.data?.record?.phase,
          )
        ) {
          operation.result = current.output;
          break;
        }
        await delay(1000);
      }
      await save("deploy_observed");
      assert.equal(operation.result?.data?.record?.phase, "succeeded");
    }
    const { HiveClient } = await load("packages/sdk/src/client.js");
    const client = new HiveClient(config.publicBaseUrl, {
      credential: owner.credential,
    });
    const status = await client.request("system.status", {});
    assert.equal(status.ready, true);
    assert.equal(status.publicBaseUrl, config.publicBaseUrl);
    const service = await client.request("serviceNodes.get", {
      serviceNodeId: manager.serviceNodeId,
    });
    assert.equal(service.hostId, config.hostId);
    assert.equal(service.serviceNodeId, manager.serviceNodeId);
    const consoleResponse = await fetch(config.publicBaseUrl + "/", {
      headers: { Authorization: "Bearer " + owner.credential },
      redirect: "error",
    });
    assert.equal(consoleResponse.status, 200);
    assert.match(
      consoleResponse.headers.get("content-type") ?? "",
      /^text\/html\b/,
    );
    assert.match(await consoleResponse.text(), /<div id="app"><\/div>/);
    report.interaction = {
      hiveReady: status.ready,
      serviceNodeId: service.serviceNodeId,
      console: "loaded",
      afterServiceRestart: true,
    };
    // Only the public core is activated. Account-, project- and target-specific services
    // remain disabled until their selected configuration is verified.
    await save("installed_core_ready_after_restart");
  } else await save("prepared_not_installed");
} catch (error) {
  report.failure = { code: error.code ?? error.name };
  await save("failed");
  process.exitCode = 1;
} finally {
  await temporaryExecutor?.close();
}
