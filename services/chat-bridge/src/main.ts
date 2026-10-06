import { fileURLToPath } from "node:url";
import {
  IvyError,
  requireThat,
  ServiceClient,
  validateChat,
} from "../../../packages/sdk/src/node.js";
import type {
  Chat,
  ToolHandler,
  Wire,
} from "../../../packages/sdk/src/node.js";
import {
  configurationPath,
  HealthFile,
  instanceConfig,
} from "../../../packages/sdk/src/host.js";
import { ChatAdmission } from "./admission.js";
import { ChatBridge } from "./bridge.js";
import { ChatMain } from "./chat-main.js";
import { ChatNativeDriver } from "./native-driver.js";
import { ChatOperations } from "./operations.js";
import { chatRegistry, chatTools } from "./registry.js";
import type { ChatTool } from "./registry.js";
import { ChatStore } from "./store.js";
import { resolveChatPlan } from "./native-plan.js";
import { WhatsAppRuntime } from "./whatsapp/runtime.js";

export async function startChatBridge(path: string) {
  const config = await instanceConfig(path);
  validateChat("Settings", config.settings);
  requireThat(
    config.componentId === "chat-bridge" && config.credential,
    "invalid_arguments",
    "ChatBridge requires its own component identity and Hive credential.",
  );
  const settings = config.settings as unknown as Chat.Settings,
    admission = new ChatAdmission(settings.definition),
    health = new HealthFile(config);
  const whatsapp = settings.whatsappConfigPath
    ? await WhatsAppRuntime.open(
        settings.whatsappConfigPath,
        config.dataRoot,
        config.hostId,
        settings.definition,
        settings.language ?? "en",
      )
    : null;
  let closed = false,
    runtime: ChatBridge | null = null,
    startedGeneration: number | null = null;
  let timer: NodeJS.Timeout | null = null,
    control: NodeJS.Timeout | null = null,
    activeWork: Promise<void> | null = null,
    healthWork: Promise<void> = Promise.resolve();
  let diagnostic: Wire.Diagnostic | null = null;
  const invocations = new Set<Promise<unknown>>();
  const retired = new Set<Promise<void>>();
  let closing: Promise<void> | null = null;
  let wakePending = false;
  const writeHealth = (
    ready: boolean,
    generation: number | null,
    detail: string,
  ) => {
    healthWork = healthWork
      .catch(() => undefined)
      .then(() => health.write(ready, detail, generation));
    return healthWork;
  };
  const kick = () => {
    if (closed || !runtime || !service.ready) return;
    if (activeWork) {
      wakePending = true;
      return;
    }
    if (timer) clearTimeout(timer);
    timer = null;
    const selected = runtime;
    activeWork = selected.worker
      .drain(() => !closed && runtime === selected && service.ready)
      .then(
        () => {
          if (runtime === selected) diagnostic = null;
        },
        (error) => {
          if (runtime !== selected) return;
          const at = new Date().toISOString(),
            code = IvyError.from(error).code;
          diagnostic = {
            code,
            resource: { workspaceId: admission.definition.workspaceId },
            severity: "warning",
            source: "chat-bridge",
            firstObservedAt:
              diagnostic?.code === code ? diagnostic.firstObservedAt : at,
            lastObservedAt: at,
            status: "current",
            message:
              "Chat work is waiting for its original storage or native outcome. Saved messages remain available.",
          };
        },
      )
      .finally(() => {
        activeWork = null;
        if (!closed && runtime && service.ready)
          timer = setTimeout(
            kick,
            wakePending || selected.worker.runnable
              ? 0
              : selected.worker.idle
                ? 30000
                : settings.pollMs,
          );
        wakePending = false;
      });
  };
  const handlers: Record<string, ToolHandler> = {};
  for (const tool of chatTools)
    handlers["chat." + tool.name] = async (args, context) => {
      const selected = runtime;
      requireThat(
        selected &&
          selected.main.native.owner.generation === context.generation,
        "service_not_ready",
        "ChatBridge has no verified current Hive connection.",
      );
      const invocation = selected.invoke(tool.name as ChatTool, args, context);
      invocations.add(invocation);
      let result: Wire.Json;
      try {
        result = await invocation;
      } finally {
        invocations.delete(invocation);
      }
      if (whatsapp && tool.name === "createMain") {
        const action = result as unknown as Chat.Operation;
        if (
          action.phase === "succeeded" &&
          action.outcome?.action === "createMain" &&
          action.request.action === "createMain" && !action.request.resumeMain
        )
          whatsapp.journal.set(
            "listener-new:" + action.outcome.binding.object.objectId,
            true,
          );
      }
      if (!tool.annotations?.readOnlyHint) {
        whatsapp?.changed();
        kick();
      }
      return result;
    };
  const service = new ServiceClient({
    publicBaseUrl: config.publicBaseUrl,
    credential: () => config.credential!,
    identity: {
      serviceNodeId: config.serviceNodeId,
      serviceName: "chat-bridge",
      hostId: config.hostId,
      version: config.version,
      buildId: config.buildId,
      hiveProtocol: 1,
      instanceMode: "singleton",
    },
    registry: chatRegistry,
    handlers,
    heartbeatMs: 2000,
    notificationFilters: () => [
      {
        namespace: "agent",
        name: "notification",
        version: "1.0.0",
        serviceNodeId: admission.definition.project.serviceNodeId,
      },
    ],
    onNotification: (value) => {
      whatsapp?.notification(value);
      const event = value.params.payload as {
        method?: string;
        params?: { turn?: { id?: string } };
      };
      if (
        event?.method === "turn/completed" &&
        typeof event.params?.turn?.id === "string" &&
        event.params.turn.id === runtime?.worker.turnId
      )
        kick();
    },
    reconcile: async (connection) => {
      const store = new ChatStore(
          connection,
          admission.definition.rootObjectId,
          config.dataRoot,
        ),
        native = new ChatNativeDriver(store, admission, {
          serviceNodeId: config.serviceNodeId,
          generation: connection.generation,
        });
      try {
        await native.verifyOwner();
        store.setRuntimeEpoch(
          (await connection.request("system.status", {})).runtimeEpoch,
        );
        if (store.rootObjectId) {
          const root = await connection.request("objects.stat", {
            objectId: store.rootObjectId,
          });
          requireThat(
            !root.effectivelyArchived,
            "chat_scope_mismatch",
            "The configured ChatBridge root must be active.",
          );
        }
        await resolveChatPlan(store, admission.definition.nativePlan);
        const next = new ChatBridge(
          new ChatMain(new ChatOperations(store, admission), native),
        );
        await next.main.find();
        connection.signal.throwIfAborted();
        runtime = next;
        diagnostic = null;
        whatsapp?.attach(next);
        connection.signal.addEventListener(
          "abort",
          () => {
            if (runtime === next) {
              runtime = null;
              whatsapp?.attach(null);
            }
            // Let the old generation's callers settle before releasing its SQLite handle.
            const work = Promise.allSettled([
              ...invocations,
              activeWork,
              whatsapp?.settled(),
            ]).then(() => store.close());
            retired.add(work);
            void work
              .finally(() => retired.delete(work))
              .catch(() => undefined);
          },
          { once: true },
        );
      } catch (error) {
        store.close();
        throw error;
      }
    },
    readiness: async (connection) => {
      requireThat(
        runtime &&
          runtime.main.native.owner.generation === connection.generation,
        "service_not_ready",
        "ChatBridge storage is not attached to this connection.",
      );
      await runtime.main.native.verifyOwner();
      await writeHealth(
        true,
        connection.generation,
        (diagnostic
          ? "Saved chat is available; original work needs reconciliation."
          : "Authenticated chat storage and original work reconciliation are available.") +
          (whatsapp ? " WhatsApp: " + whatsapp.transport.state + "." : ""),
      );
      return { ready: true, diagnostics: diagnostic ? [diagnostic] : [] };
    },
    onState: (state) => {
      if (state.status === "ready" && state.generation !== startedGeneration) {
        startedGeneration = state.generation ?? null;
        kick();
      } else if (["offline", "stopped", "connecting"].includes(state.status)) {
        runtime = null;
        whatsapp?.attach(null);
        diagnostic = null;
        startedGeneration = null;
        if (timer) clearTimeout(timer);
        timer = null;
        void writeHealth(
          false,
          state.generation ?? null,
          "Hive ChatBridge connection is not ready.",
        ).catch(() => undefined);
      }
    },
  });
  const close = () =>
    (closing ??= (async () => {
      closed = true;
      if (timer) clearTimeout(timer);
      if (control) clearInterval(control);
      try {
        await whatsapp?.close();
      } finally {
        await service.stop();
        await activeWork;
        await Promise.all(retired);
      }
      await healthWork.catch(() => undefined);
      await health.write(false, "ChatBridge stopped.");
    })());
  await writeHealth(
    false,
    null,
    "Connecting ChatBridge and verifying its exact workspace contracts.",
  );
  let checkingControl = false;
  control = setInterval(() => {
    if (closed || checkingControl) return;
    checkingControl = true;
    void health
      .control()
      .then((value) => (value ? close() : undefined))
      .catch(() => undefined)
      .finally(() => {
        checkingControl = false;
      });
  }, 500);
  service.start();
  whatsapp?.start();
  return {
    service,
    close,
    get runtime() {
      return runtime;
    },
    whatsapp,
  };
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    const bridge = await startChatBridge(configurationPath());
    const stop = () => {
      void bridge.close().then(
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
        message: "ChatBridge failed to start.",
      }) + "\n",
    );
    process.exitCode = 1;
  }
}
