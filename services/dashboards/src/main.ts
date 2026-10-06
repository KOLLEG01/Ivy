import { fileURLToPath } from "node:url";
import { z } from "zod";
import {
  deriveOperationId,
  IvyError,
  requireThat,
  ServiceClient,
} from "../../../packages/sdk/src/node.js";
import type { ToolHandler, Wire } from "../../../packages/sdk/src/node.js";
import {
  configurationPath,
  HealthFile,
  instanceConfig,
} from "../../../packages/sdk/src/host.js";
import { dashboardRegistry, inputs } from "./schema.js";
import { DashboardStore } from "./store.js";
import { DashboardRenderer } from "./render.js";

export async function startDashboards(path: string) {
  const config = await instanceConfig(path);
  requireThat(
    config.componentId === "dashboards" && config.credential,
    "invalid_arguments",
    "Dashboards requires its own identity and Hive credential.",
  );
  const settings = z
    .object({
      rootObjectId: z.string().nullable().default(null),
      chromiumExecutable: z.string().min(1).optional(),
    })
    .strict()
    .parse(config.settings);
  const health = new HealthFile(config),
    renderer = new DashboardRenderer(settings.chromiumExecutable);
  let store: DashboardStore | null = null,
    closed = false;
  let healthWork: Promise<void> = Promise.resolve();
  const writeHealth = (
    ready: boolean,
    detail: string,
    generation: number | null = null,
  ) =>
    (healthWork = healthWork
      .catch(() => {})
      .then(() => health.write(ready, detail, generation)));
  const handlers: Record<string, ToolHandler> = {};
  for (const name of Object.keys(inputs) as (keyof typeof inputs)[])
    handlers["dashboards." + name] = async (args, context) => {
      requireThat(store, "service_not_ready", "Dashboards is connecting.");
      const parsed = inputs[name].safeParse(args);
      requireThat(
        parsed.success,
        "invalid_arguments",
        parsed.error?.message ?? "Invalid dashboard request.",
      );
      if (name === "save" || name === "delete")
        requireThat(
          context.operationId,
          "invalid_arguments",
          "An operationId is required for writes.",
        );
      const operation = context.operationId
        ? deriveOperationId(context.operationId, {
            service: "dashboards",
            caller: context.callerPrincipalId,
          })
        : "";
      if (name === "list") {
        const a = inputs.list.parse(args);
        return await store.list(a.cursor, a.limit);
      }
      if (name === "save") {
        const a = inputs.save.parse(args);
        return await store.save(a.value, a.id, a.expectedRevision, operation);
      }
      if (name === "delete") {
        const a = inputs.delete.parse(args);
        return await store.remove(a.id, a.expectedRevision, operation);
      }
      const id = args["id"] as string,
        row =
          name === "image"
            ? await store.image(id, args["token"] as string)
            : await store.read(id);
      if (name === "read") return row as unknown as Wire.Json;
      const data = await store.data(row.value);
      if (name === "data")
        return {
          data,
          dashboard: row,
          observedAt: new Date().toISOString(),
        } as unknown as Wire.Json;
      const options =
        name === "image"
          ? (() => {
              const {
                id: _id,
                token: _token,
                ...output
              } = inputs.image.parse(args);
              return output;
            })()
          : (() => {
              const { id: _id, ...output } = inputs.render.parse(args);
              return output;
            })();
      return {
        ...(await renderer.render(row.value.html, data, options)),
        metadata: {
          ...row.value.metadata,
          refreshSeconds: row.value.refreshSeconds,
        },
      };
    };
  const service = new ServiceClient({
    publicBaseUrl: config.publicBaseUrl,
    credential: () => config.credential!,
    identity: {
      serviceNodeId: config.serviceNodeId,
      hostId: config.hostId,
      serviceName: "dashboards",
      version: config.version,
      buildId: config.buildId,
      hiveProtocol: 1,
    },
    registry: dashboardRegistry,
    handlers,
    heartbeatMs: 2000,
    reconcile: async (connection) => {
      if (settings.rootObjectId)
        await connection.request("objects.stat", {
          objectId: settings.rootObjectId,
        });
      try {
        await renderer.start();
      } catch (error) {
        throw new IvyError(
          "dependency_unavailable",
          "Chromium startup failed: " + String(error).slice(0, 240),
        );
      }
      store = new DashboardStore(
        connection,
        settings.rootObjectId,
        config.publicBaseUrl,
        config.serviceNodeId,
      );
    },
    readiness: async (connection) => {
      await writeHealth(
        true,
        "Dashboard storage and image renderer ready.",
        connection.generation,
      );
      return { ready: true, diagnostics: [] };
    },
    onState: (state) => {
      if (["offline", "stopped", "connecting"].includes(state.status)) {
        store = null;
        void writeHealth(
          false,
          state.message ?? "Hive connection unavailable.",
        );
      }
    },
  });
  const close = async () => {
    if (closed) return;
    closed = true;
    clearInterval(control);
    await service.stop();
    await renderer.close();
    await writeHealth(false, "Dashboards stopped.");
  };
  let checking = false;
  const control = setInterval(() => {
    if (!checking && !closed) {
      checking = true;
      void health
        .control()
        .then((value) => (value ? close() : undefined))
        .finally(() => {
          checking = false;
        })
        .catch(() => {});
    }
  }, 500);
  await writeHealth(false, "Connecting Dashboards.");
  service.start();
  return { service, close };
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const runtime = await startDashboards(configurationPath()).catch((error) => {
    process.stderr.write(IvyError.from(error).message + "\n");
    process.exitCode = 1;
    return null;
  });
  if (runtime)
    for (const signal of ["SIGTERM", "SIGINT"])
      process.on(signal, () => {
        void runtime.close();
      });
}
