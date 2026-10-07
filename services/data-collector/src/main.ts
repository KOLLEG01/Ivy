import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { randomUUID, timingSafeEqual } from "node:crypto";
import { createServer } from "node:http";
import {
  ServiceClient,
  requireThat,
  IvyError,
} from "../../../packages/sdk/src/node.js";
import type { Wire } from "../../../packages/sdk/src/node.js";
import {
  configurationPath,
  HealthFile,
  instanceConfig,
} from "../../../packages/sdk/src/host.js";
import { CollectorEngine } from "./engine.js";
import { CollectorStore } from "./store.js";
import { TaskRunner } from "./runner.js";
import { settingsSchema, registry } from "./schema.js";
import { NativeMcpSession } from "./native-mcp.js";

export async function startDataCollector(path: string) {
  const config = await instanceConfig(path),
    settings = settingsSchema.parse(config.settings);
  requireThat(
    config.componentId === "data-collector" && config.credential,
    "invalid_arguments",
    "DataCollector needs its own component identity and credential.",
  );
  const health = new HealthFile(config),
    store = new CollectorStore(join(config.dataRoot, "data-collector.sqlite"));
  const engine: CollectorEngine = new CollectorEngine(
    store,
    new TaskRunner(
      join(config.workRoot ?? config.dataRoot, "tasks"),
      config.artifactRoot,
      settings,
      (binding, signal) => {
        requireThat(
          engine.client,
          "provider_unavailable",
          "Hive connection is not ready for MCP calls.",
        );
        return new NativeMcpSession(engine.client, binding, signal);
      },
    ),
    settings,
  );
  let closed = false,
    tickWork: Promise<void> | null = null,
    healthWork: Promise<void> = Promise.resolve();
  const writeHealth = (
    ready: boolean,
    detail: string,
    generation: number | null = null,
  ) => {
    healthWork = healthWork
      .catch(() => undefined)
      .then(() => health.write(ready, detail, generation));
    return healthWork;
  };
  const service = new ServiceClient({
    publicBaseUrl: config.publicBaseUrl,
    credential: () => config.credential!,
    identity: {
      serviceNodeId: config.serviceNodeId,
      serviceName: "data-collector",
      instanceMode: "multiple",
      hostId: config.hostId,
      version: config.version,
      buildId: config.buildId,
      hiveProtocol: 1,
    },
    registry,
    handlers: {
      "data-collector.read": async (args) =>
        (await engine.read(args)) as Wire.Json,
      "data-collector.update": (args, context) =>
        engine.update(args, context) as Wire.Json,
    },
    reconcile: (connection) => engine.attach(connection),
    readiness: async (connection) => {
      await writeHealth(
        true,
        "Collector storage, scripts and result routing are ready.",
        connection.generation,
      );
      return { ready: true, diagnostics: [] };
    },
    onState: (state) => {
      if (["connecting", "offline", "stopped"].includes(state.status)) {
        engine.detach();
        void writeHealth(false, "Hive connection is not ready.").catch(
          () => undefined,
        );
      }
    },
    heartbeatMs: 2000,
  });
  const authorized = (
    row: ReturnType<CollectorStore["get"]>,
    supplied: string,
  ) => {
    if (!row?.task.enabled) return false;
    if (row.task.allowUnauthenticatedInput === true) return true;
    const expected = row.task.inputSecretName
      ? settings.secrets[row.task.inputSecretName]
      : null;
    return Boolean(
      expected &&
      Buffer.byteLength(expected) === Buffer.byteLength(supplied) &&
      timingSafeEqual(Buffer.from(expected), Buffer.from(supplied)),
    );
  };
  const ingress = settings.ingress
    ? createServer(async (request, response) => {
        try {
          const path = request.url ?? "",
            id =
              /^\/tasks\/([a-z0-9][a-z0-9_-]{0,63})\/run$/.exec(path)?.[1] ??
              settings.ingress!.routes?.[path],
            row = typeof id === "string" ? store.get(id) : null;
          const supplied =
            request.headers.authorization?.replace(/^Bearer /, "") ?? "";
          if (request.method !== "POST" || !authorized(row, supplied)) {
            response.writeHead(401).end();
            request.resume();
            return;
          }
          if (closed) {
            response.writeHead(503).end();
            request.resume();
            return;
          }
          const chunks: Buffer[] = [];
          let bytes = 0;
          for await (const chunk of request) {
            bytes += chunk.length;
            if (bytes > 262144) {
              response.writeHead(413).end();
              request.destroy();
              return;
            }
            chunks.push(chunk);
          }
          const input = JSON.parse(Buffer.concat(chunks).toString("utf8"));
          const current = id ? store.get(id) : null;
          if (
            !authorized(current, supplied) ||
            current?.revision !== row?.revision
          ) {
            response.writeHead(401).end();
            return;
          }
          if (closed) {
            response.writeHead(503).end();
            return;
          }
          const result = engine.update({
            action: "run",
            id,
            operationId: request.headers["idempotency-key"] ?? randomUUID(),
            input,
          });
          response
            .writeHead(202, { "Content-Type": "application/json" })
            .end(JSON.stringify(result));
        } catch (error) {
          response
            .writeHead(IvyError.from(error).code === "task_busy" ? 409 : 400, {
              "Content-Type": "application/json",
            })
            .end(JSON.stringify({ error: IvyError.from(error).code }));
        }
      })
    : null;
  if (ingress) {
    ingress.requestTimeout = 15000;
    ingress.headersTimeout = 10000;
    await new Promise<void>((resolve, reject) => {
      ingress.once("error", reject);
      ingress.listen(settings.ingress!.port, settings.ingress!.host, resolve);
    });
  }
  const timer = setInterval(() => {
    if (closed || !service.ready || tickWork) return;
    tickWork = engine
      .tick()
      .catch(() => undefined)
      .finally(() => {
        tickWork = null;
      });
  }, 500);
  let checking = false;
  const control = setInterval(() => {
    if (closed || checking) return;
    checking = true;
    void health
      .control()
      .then((value) => (value ? close() : undefined))
      .catch(() => undefined)
      .finally(() => {
        checking = false;
      });
  }, 500);
  const close = async () => {
    if (closed) return;
    closed = true;
    clearInterval(timer);
    clearInterval(control);
    ingress?.closeAllConnections();
    ingress?.close();
    const stopped = engine.close();
    await service.stop();
    await tickWork;
    await stopped;
    await healthWork.catch(() => undefined);
    store.close();
    await health.write(false, "DataCollector stopped.");
  };
  await writeHealth(false, "Connecting DataCollector.");
  service.start();
  return { service, engine, close };
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    const runtime = await startDataCollector(configurationPath());
    const stop = () => {
      void runtime.close().catch(() => {
        process.exitCode = 1;
      });
    };
    process.on("SIGTERM", stop);
    process.on("SIGINT", stop);
  } catch (error) {
    process.stderr.write(
      JSON.stringify({
        code: IvyError.from(error).code,
        message: "DataCollector failed to start.",
      }) + "\n",
    );
    process.exitCode = 1;
  }
}
