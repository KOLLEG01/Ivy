import { join, isAbsolute } from "node:path";
import { fileURLToPath } from "node:url";
import { cli } from "../../../packages/cli/src/main.js";
import {
  consumeEvents,
  digest,
  IvyError,
  requireThat,
  ServiceClient,
} from "../../../packages/sdk/src/node.js";
import type {
  ServiceConnection,
  ToolHandler,
  Wire,
} from "../../../packages/sdk/src/node.js";
import {
  atomicJson,
  bundledHostSchema,
  configurationPath,
  freshExecutorStatus,
  HealthFile,
  HostJournal,
  hostConfig,
  instanceConfig,
  jsonFile,
  RuntimeOwner,
  runtimeResetActive,
  validateHost,
} from "../../../packages/sdk/src/host.js";
import type { Host } from "../../../packages/sdk/src/host.js";

export const managerTools: Wire.ToolDefinition[] = [
  [
    "status",
    "ManagerStatusInput",
    "HostStatus",
    "Check this host’s installed services, builds, readiness and deployment activity.",
  ],
  [
    "deployment",
    "ManagerDeploymentInput",
    "JournalEntry",
    "Read progress and outcome of an original deployment on this host.",
  ],
  [
    "deploy",
    "ManagerDeployInput",
    "DeploymentRecord",
    "Deploy a validated candidate or configured local source. Acceptance starts the operation; use host.deployment to check readiness.",
  ],
  [
    "rollback",
    "ManagerRollbackInput",
    "DeploymentRecord",
    "Roll back to a selected compatible build retained on this host; check host.deployment for the outcome.",
  ],
  ...["restart", "enable", "disable"].map((name) => [
    name,
    "ManagerLifecycleInput",
    "DeploymentRecord",
    "Request " + name + " for one configured service instance on this host; check host.deployment for the outcome.",
  ]),
].map(([name, input, output, description]) => ({
  namespace: "host",
  name: name!,
  interfaceVersion: "1.0.0",
  description: description!,
  inputSchema: bundledHostSchema("#/$defs/" + input),
  outputSchema: bundledHostSchema(
    "#/$defs/" + output,
    output === "DeploymentRecord"
      ? "https://ivy.invalid/schemas/hive-wire.schema.json"
      : "https://ivy.invalid/schemas/host.schema.json",
  ),
  annotations: {
    readOnlyHint: ["status", "deployment"].includes(name!),
    idempotentHint: true,
  },
}));

function hostReportIdentity(status: Host.HostStatus): string {
  const { observedAt: _observedAt, executor, instances, ...stable } = status;
  return JSON.stringify({
    ...stable,
    executor: executor ? { ...executor, observedAt: "" } : null,
    instances: instances.map((instance) => ({ ...instance, observedAt: null })),
  });
}

export async function startServiceManager(
  path: string,
): Promise<{ service: ServiceClient; close: () => Promise<void> }> {
  const config = await instanceConfig(path);
  validateHost("ManagerSettings", config.settings);
  requireThat(
    config.componentId === "service-manager" && config.credential,
    "invalid_arguments",
    "ServiceManager requires its own component identity and Hive credential.",
  );
  const settings = config.settings as unknown as Host.ManagerSettings;
  requireThat(
    isAbsolute(settings.hostConfigPath),
    "invalid_arguments",
    "ServiceManager needs an explicit absolute local host configuration.",
  );
  const initial = await hostConfig(settings.hostConfigPath);
  requireThat(
    !(await runtimeResetActive(initial)),
    "maintenance_active",
    "Service ownership remains stopped for an incomplete runtime reset.",
  );
  requireThat(
    initial.hostId === config.hostId &&
      initial.instances.some(
        (value) =>
          value.instanceId === config.instanceId &&
          value.serviceNodeId === config.serviceNodeId &&
          value.componentId === config.componentId,
      ),
    "target_conflict",
    "ServiceManager is not configured on its claimed local host.",
  );
  const journal = new HostJournal(initial),
    health = new HealthFile(config);
  // A Windows host has one local owner for each assigned process. Docker Hive and the
  // independently bootstrapped executor keep their own external owners.
  const owners = new Map<string, RuntimeOwner>();
  try {
    if (process.platform === "win32")
      for (const instance of initial.instances) {
        if (
          ["hive", "host-executor", "service-manager"].includes(
            instance.componentId,
          ) ||
          instance.engine !== "process"
        )
          continue;
        const owner = new RuntimeOwner(
          initial,
          instance.instanceId,
          config.artifactRoot,
        );
        owner.start();
        owners.set(instance.instanceId, owner);
      }
  } catch (error) {
    for (const owner of [...owners.values()].reverse())
      await owner.close().catch(() => undefined);
    journal.close();
    throw error;
  }
  let closed = false,
    minimumSequence = 0,
    lastHostReport: string | null = null,
    lastDeploymentReport: string | null = null;
  let healthWork: Promise<void> = Promise.resolve();
  const writeHealth = (
    ready: boolean,
    generation: number | null,
    message: string,
  ) => {
    healthWork = healthWork
      .catch(() => undefined)
      .then(() => health.write(ready, message, generation));
    return healthWork;
  };
  const refresh = async () => {
    requireThat(!closed, "service_unavailable", "ServiceManager is stopping.");
    const value = await jsonFile<Host.ExecutorStatus>(
      join(journal.config.runtimeRoot, "executor.json"),
    ).catch(() => null);
    if (value) {
      try {
        validateHost("ExecutorStatus", value);
        requireThat(
          value.hostId === config.hostId,
          "target_conflict",
          "Executor status belongs to another host.",
        );
        requireThat(
          freshExecutorStatus(value),
          "service_not_ready",
          "Executor status is stale.",
        );
      } catch {
        return null;
      }
    }
    return value;
  };
  const reloadForAction = async () => {
    const next = await hostConfig(settings.hostConfigPath);
    requireThat(
      next.hostId === config.hostId,
      "target_conflict",
      "Reloaded host configuration belongs to another host.",
    );
    journal.useConfiguration(next);
  };
  const publish = async (
    connection: ServiceConnection,
  ): Promise<Host.ExecutorStatus | null> => {
    const executor = await refresh(),
      status = journal.status(executor),
      hostIdentity = hostReportIdentity(status);
    if (hostIdentity !== lastHostReport) {
      const snapshot = journal.managementSnapshot(
        executor,
        minimumSequence,
        status,
      );
      await connection.request("hosts.report", snapshot);
      minimumSequence = snapshot.sequence;
      lastHostReport = hostIdentity;
    }
    const records = journal.list(200).map((value) => value.record),
      deploymentIdentity = JSON.stringify(records);
    if (deploymentIdentity !== lastDeploymentReport) {
      await connection.request("deployments.report", { records });
      lastDeploymentReport = deploymentIdentity;
    }
    return executor;
  };
  const handlers: Record<string, ToolHandler> = {
    "host.status": async () =>
      journal.status(await refresh()) as unknown as Wire.Json,
    "host.deployment": (args) =>
      journal.get(String(args["deploymentId"])) as unknown as Wire.Json,
  };
  for (const action of ["deploy", "rollback", "restart", "enable", "disable"])
    handlers["host." + action] = async (args, context) => {
      requireThat(
        !context.operationId || context.operationId === args["operationId"],
        "mutation_conflict",
        "Tool and local action operation IDs must match.",
      );
      await reloadForAction();
      const command = [
        action,
        "--config",
        settings.hostConfigPath,
        "--instance",
        String(args["instanceId"]),
        "--operation-id",
        String(args["operationId"]),
        "--json",
      ];
      if (args["candidateId"])
        command.push("--candidate", String(args["candidateId"]));
      if (args["source"]) command.push("--source", String(args["source"]));
      if (args["targetBuild"])
        command.push("--to", String(args["targetBuild"]));
      const result = await cli(command);
      // A terminal failure still has a definite durable deployment identity and phase.
      if (result.output.deploymentId) {
        const entry = journal.get(result.output.deploymentId);
        return entry.record as unknown as Wire.Json;
      }
      requireThat(
        result.exitCode === 0,
        result.output.code,
        "Local deployment acceptance failed; inspect the exact host request.",
      );
      throw new IvyError(
        "outcome_unknown",
        "Local command did not return a durable deployment identity.",
        "unknown",
      );
    };
  const service = new ServiceClient({
    publicBaseUrl: config.publicBaseUrl,
    credential: () => config.credential!,
    identity: {
      serviceNodeId: config.serviceNodeId,
      serviceName: "service-manager",
      hostId: config.hostId,
      version: config.version,
      buildId: config.buildId,
      hiveProtocol: 1,
    },
    registry: () => ({
      discoveryHint: "",
      namespaces: [
        {
          namespace: "host",
          description: "Host-local deployment and recovery",
          guideMarkdown:
            "Select the host that owns the service and read host.status for instance IDs, builds and unfinished operations. Deploy a validated candidate or configured source, then follow host.deployment until the intended build is ready. Operations continue in the local executor after disconnection; keep operationId and inspect the original deployment before retrying.",
          tools: managerTools,
          topics: [],
          inventoryKinds: [],
        },
      ],
      contracts: [],
      requiredContracts: [],
    }),
    handlers,
    heartbeatMs: 2000,
    reconcile: async (connection) => {
      const prior = await connection.request("hosts.observations", {
        hostId: config.hostId,
      });
      minimumSequence = Math.max(
        minimumSequence,
        ...prior.items.map((value) => value.snapshot.sequence),
        0,
      );
      lastHostReport = prior.items[0]
        ? hostReportIdentity(prior.items[0].snapshot.status)
        : null;
      lastDeploymentReport = null;
      await publish(connection);
      const catalog = await connection.request("packages.catalog", {
        after: 0,
        includeUis: true,
      });
      const catalogObjectId = catalog.objectId;
      const configuration = await connection.request("hostConfigurations.get", {
        hostId: config.hostId,
      });
      const objectIds = [configuration.objectId, catalogObjectId];
      const subscription = {
        name: "host-desired-" + digest(objectIds.join(":")).slice(7, 31),
        filter: { topics: ["hive.object.changed"], objectIds },
      };
      void consumeEvents(
        connection,
        subscription,
        async (batch) => {
          if (!batch.gap && batch.items.length === 0) return;
          await atomicJson(
            join(journal.config.runtimeRoot, "desired-state-hint.json"),
            {
              schemaVersion: 1,
              throughSequence: batch.throughSequence,
              observedAt: new Date().toISOString(),
              configuration: Boolean(
                batch.gap ||
                batch.items.some(
                  (item) =>
                    (item.payload as { objectId?: string }).objectId ===
                    configuration.objectId,
                ),
              ),
              packages: Boolean(
                batch.gap ||
                batch.items.some(
                  (item) =>
                    (item.payload as { objectId?: string }).objectId ===
                    catalogObjectId,
                ),
              ),
            },
          );
        },
        { pollMs: 60_000 },
      ).catch((error) => {
        if (!connection.signal.aborted) connection.close(IvyError.from(error));
      });
    },
    readiness: async (connection) => {
      const executor = await publish(connection),
        ready = Boolean(
          executor?.state === "ready" &&
          Date.now() - Date.parse(executor.observedAt) < 15_000 &&
          Date.parse(executor.observedAt) <= Date.now() + 1000,
        );
      const now = new Date().toISOString(),
        message = ready
          ? "Independent executor and current host reports verified."
          : "Independent executor status is missing, stale or not ready.";
      await writeHealth(ready, connection.generation, message);
      return {
        ready,
        diagnostics: [
          {
            code: "host_executor_unavailable",
            resource: { hostId: config.hostId },
            source: config.serviceNodeId,
            severity: ready ? ("info" as const) : ("error" as const),
            status: ready ? ("resolved" as const) : ("current" as const),
            firstObservedAt: now,
            lastObservedAt: now,
            message,
          },
        ],
      };
    },
    onState: (state) => {
      if (["offline", "stopped", "connecting"].includes(state.status))
        void writeHealth(
          false,
          state.generation ?? null,
          "Hive connection is not reconciled" +
            (state.code ? ": " + state.code : "") +
            ".",
        ).catch(() => undefined);
    },
  });
  let timer: NodeJS.Timeout | null = null,
    controlBusy = false;
  const close = async () => {
    if (closed) return;
    closed = true;
    if (timer) clearInterval(timer);
    try {
      for (const owner of [...owners.values()].reverse()) await owner.close();
      await service.stop();
      await healthWork.catch(() => undefined);
      await health.write(false, "ServiceManager stopped.");
    } finally {
      journal.close();
    }
  };
  try {
    await writeHealth(false, null, "Connecting and reconciling host state.");
  } catch (error) {
    closed = true;
    for (const owner of [...owners.values()].reverse())
      await owner.close().catch(() => undefined);
    await healthWork.catch(() => undefined);
    journal.close();
    throw error;
  }
  timer = setInterval(() => {
    if (controlBusy || closed) return;
    controlBusy = true;
    void health
      .control()
      .then((value) => {
        if (value) return close();
      })
      .catch(() => undefined)
      .finally(() => {
        controlBusy = false;
      });
  }, 500);
  service.start();
  return { service, close };
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    const manager = await startServiceManager(configurationPath());
    const stop = () => {
      void manager.close().then(
        () => {
          process.exitCode = 0;
        },
        () => {
          process.exitCode = 1;
        },
      );
    };
    process.on("SIGTERM", stop);
    process.on("SIGINT", stop);
  } catch (error) {
    process.stderr.write(
      JSON.stringify({
        code: IvyError.from(error).code,
        message: "ServiceManager failed to start.",
      }) + "\n",
    );
    process.exitCode = 1;
  }
}
