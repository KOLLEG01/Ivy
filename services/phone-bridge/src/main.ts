import { fileURLToPath } from "node:url";
import { randomUUID } from "node:crypto";
import {
  IvyError,
  requireThat,
  ServiceClient,
} from "../../../packages/sdk/src/node.js";
import type { ToolHandler } from "../../../packages/sdk/src/node.js";
import {
  configurationPath,
  HealthFile,
  instanceConfig,
} from "../../../packages/sdk/src/host.js";
import { PhoneAdmission } from "../../../services/phone-bridge/src/runtime/admission.js";
import { PhoneCallCommands } from "../../../services/phone-bridge/src/runtime/calls.js";
import { PhoneFlow } from "../../../services/phone-bridge/src/runtime/flow.js";
import { PhoneJournal } from "../../../services/phone-bridge/src/runtime/journal.js";
import { PhoneBridge } from "../../../services/phone-bridge/src/runtime/bridge.js";
import { PhoneDiagnostics } from "../../../services/phone-bridge/src/runtime/diagnostics.js";
import { startPhoneProcess } from "../../../services/phone-bridge/src/runtime/process.js";
import {
  phoneRegistry,
  phoneTools,
} from "../../../services/phone-bridge/src/runtime/registry.js";
import {
  phoneCredentials,
  phoneSettings,
} from "../../../services/phone-bridge/src/runtime/settings.js";
import { validatePhone } from "../../../services/phone-bridge/src/runtime/native.js";
import type { PhoneNativeClient } from "../../../services/phone-bridge/src/runtime/native.js";
import { PhoneCallLogs } from "../../../services/phone-bridge/src/runtime/call-logs.js";
import { PhoneCodexVoice } from "./runtime/codex-voice.js";

export async function startPhoneBridge(path: string) {
  const config = await instanceConfig(path),
    settings = phoneSettings(config.settings);
  requireThat(
    config.componentId === "phone-bridge" && config.credential,
    "invalid_arguments",
    "PhoneBridge requires its own component identity and Hive credential.",
  );
  const health = new HealthFile(config),
    admission = new PhoneAdmission(settings.policy);
  const journal = new PhoneJournal(
    config.dataRoot,
    { hostId: config.hostId, serviceNodeId: config.serviceNodeId },
    undefined,
    Number(settings.binding["maxConcurrentCalls"] ?? 1),
  );
  const logs = new PhoneCallLogs(journal.dataRoot);
  let processOwner: Awaited<ReturnType<typeof startPhoneProcess>> | null = null,
    flow: PhoneFlow | null = null;
  let service: ServiceClient | null = null,
    timer: NodeJS.Timeout | null = null;
  let active: Promise<void> | null = null,
    closing: Promise<void> | null = null,
    stopped = false;
  let voiceWork: Promise<void> | null = null,
    lastVoiceCheck = 0;
  let rotating = false;
  const diagnostics = new PhoneDiagnostics(config.serviceNodeId);
  let healthWork: Promise<void> = Promise.resolve();
  const writeHealth = (
    ready: boolean,
    generation: number | null,
    details: string,
  ) => {
    const archive = journal.archiveStatus(),
      active = journal.status();
    const warning =
      active.operations >= journal.limits.maxOperations * 0.8 ||
      active.reservedBytes >= journal.limits.maxBytes * 0.8
        ? " Active journal near capacity: investigate unknown calls; never delete replay evidence."
        : "";
    healthWork = healthWork
      .catch(() => undefined)
      .then(() =>
        health.write(
          ready,
          details +
            ` Receipt archive: ${archive.records} records, ${archive.bytes} bytes.` +
            warning,
          generation,
        ),
      );
    return healthWork;
  };
  const checkVoice = () => {
    if (
      !settings.codexVoice ||
      !settings.incomingPrincipalId ||
      !flow ||
      stopped ||
      voiceWork
    )
      return;
    const selected = flow;
    lastVoiceCheck = Date.now();
    voiceWork = selected
      .prewarmVoice(settings.incomingPrincipalId)
      .then(
        () => {
          if (selected === flow && diagnostics.voiceSucceeded())
            process.stdout.write(
              JSON.stringify({ event: "phone_codex_voice_recovered" }) + "\n",
            );
        },
        (error) => {
          if (selected !== flow || stopped) return;
          const code = IvyError.from(error).code;
          if (diagnostics.voiceFailed(code))
            process.stderr.write(
              JSON.stringify({ event: "phone_codex_voice_unavailable", code }) +
                "\n",
            );
        },
      )
      .finally(() => {
        voiceWork = null;
      });
  };
  const close = (): Promise<void> => {
    if (closing) return closing;
    stopped = true;
    if (timer) clearTimeout(timer);
    closing = (async () => {
      try {
        await service?.stop();
        try {
          await flow?.close();
        } finally {
          await processOwner?.close();
        }
      } finally {
        await active?.catch(() => undefined);
        await Promise.allSettled(
          [voiceWork].filter((work): work is Promise<void> => work !== null),
        );
        await healthWork.catch(() => undefined);
        logs.close();
        journal.close();
        await health.write(false, "PhoneBridge stopped.");
      }
    })();
    return closing;
  };
  try {
    await writeHealth(
      false,
      null,
      "Starting the exclusive native Phone owner.",
    );
    let native: Awaited<ReturnType<typeof startPhoneProcess>>["client"];
    let bridge: PhoneBridge;
    const initialize = async () => {
      processOwner = await startPhoneProcess({
        artifactRoot: config.artifactRoot,
        ...settings.native,
        journal,
      });
      if (stopped) {
        await processOwner.close();
        throw new IvyError(
          "service_not_ready",
          "Phone stopped during process initialization.",
        );
      }
      native = processOwner.client;
      const credentials = await phoneCredentials(settings.credentialsPath);
      const configuration = {
        binding: settings.binding,
        codecs: settings.codecs,
        incomingPeers: [
          ...new Set(
            [
              ...settings.policy.incoming,
              ...(settings.policy.challengedIncoming ?? []),
            ].map((item) => item.peerAddress),
          ),
        ],
        registration: settings.registration,
        password: settings.registration ? credentials.password : null,
      };
      validatePhone("Configure", configuration);
      const configured = await native.request(
        "configure",
        configuration,
        randomUUID(),
      );
      requireThat(
        configured.ok &&
          configured.result !== null &&
          typeof configured.result === "object" &&
          "configured" in configured.result &&
          configured.result.configured === true,
        "phone_configuration_failed",
        "Original native configuration did not complete.",
      );
      const voice = settings.codexVoice
        ? new PhoneCodexVoice(
            settings.codexVoice,
            journal,
            {
              artifactRoot: config.artifactRoot,
              dataRoot: config.dataRoot,
              credential: config.credential!,
            },
            (code) => diagnostics.voiceFailed(code),
          )
        : null;
      flow = new PhoneFlow(
        new PhoneCallCommands(admission, journal, native),
        native,
        settings,
        () => phoneCredentials(settings.credentialsPath),
        (callId, code) => {
          diagnostics.callFailed(callId, code);
          process.stderr.write(
            JSON.stringify({ event: "phone_call_failed", callId, code }) + "\n",
          );
        },
        (callId) => diagnostics.callSucceeded(callId),
        voice,
        logs,
      );
      // SIP starts independently; prepare the configured caller's durable task off the call path.
      checkVoice();
      bridge = new PhoneBridge(flow, native);
      const original = processOwner;
      process.stdout.write(
        JSON.stringify({ event: "phone_native_started", epoch: native.epoch }) +
          "\n",
      );
      void native.closed
        .then((code) => {
          process.stderr.write(
            JSON.stringify({
              event: "phone_native_closed",
              epoch: original.client.epoch,
              code,
            }) + "\n",
          );
          if (!rotating && processOwner === original) return close();
        })
        .catch(() => {
          process.exitCode = 1;
        });
    };
    await initialize();
    const handlers: Record<string, ToolHandler> = {};
    for (const tool of phoneTools)
      handlers["phone." + tool.name] = (args, context) => {
        requireThat(
          !stopped &&
            !rotating &&
            service?.ready &&
            service.connection?.generation === context.generation,
          "service_not_ready",
          "Phone invocation belongs to an unavailable Hive generation.",
        );
        return bridge.invoke(tool.name, args, context);
      };
    service = new ServiceClient({
      publicBaseUrl: config.publicBaseUrl,
      credential: () => config.credential!,
      identity: {
        serviceNodeId: config.serviceNodeId,
        serviceName: "phone-bridge",
        hostId: config.hostId,
        version: config.version,
        buildId: config.buildId,
        hiveProtocol: 1,
        instanceMode: "singleton",
      },
      registry: phoneRegistry,
      handlers,
      heartbeatMs: 2000,
      reconcile: async () => {
        requireThat(
          !stopped && !rotating && journal.epoch === native.epoch,
          "service_not_ready",
          "Original native Phone owner has retired.",
        );
      },
      readiness: async (connection) => {
        requireThat(
          !stopped && !rotating && journal.epoch === native.epoch,
          "service_not_ready",
          "Original native Phone owner is unavailable.",
        );
        const currentDiagnostics = diagnostics.snapshot(
          journal.currentCalls().map((call) => call.callId),
        );
        const voiceIssue = currentDiagnostics.find(
          (item) =>
            item.resource !== null &&
            typeof item.resource === "object" &&
            !Array.isArray(item.resource) &&
            item.resource.scope === "codex_voice",
        );
        await writeHealth(
          true,
          connection.generation,
          voiceIssue
            ? `Codex Voice unavailable (${voiceIssue.code}); SIP remains active.`
            : currentDiagnostics.length
              ? "Original Phone results remain available; Phone needs attention."
              : "Phone calls and original outcome queries are available.",
        );
        return { ready: true, diagnostics: currentDiagnostics };
      },
      onState: (state) => {
        if (["offline", "stopped", "connecting"].includes(state.status))
          void writeHealth(
            !stopped && !rotating && journal.epoch === native.epoch,
            state.generation ?? null,
            `Local Phone owner remains active during Hive reconnect; Hive tools are unavailable${state.code ? ` (${state.code})` : ""}.`,
          ).catch(() => undefined);
      },
    });
    const tick = () => {
      if (stopped || active) return;
      active = (async () => {
        if (await health.control()) {
          queueMicrotask(() => {
            void close().catch(() => {
              process.exitCode = 1;
            });
          });
          return;
        }
        const observed = await native.observe();
        diagnostics.registrationObserved(
          settings.registration !== null,
          observed.registration,
        );
        diagnostics.pollSucceeded();
        // SIP admission is local to this fenced native owner. A Hive reconnect
        // must not consume the provider's ringing window.
        if (
          !stopped &&
          !rotating &&
          journal.epoch === native.epoch &&
          settings.incomingPrincipalId
        ) {
          for (const id of observed.callIds ??
            (observed.call ? [observed.call.id] : [])) {
            if (
              journal.getCall(id) ||
              journal.currentCalls().length >= journal.maxConcurrentCalls
            )
              continue;
            const candidate =
              observed.call?.id === id ? observed : await native.observe(id);
            if (
              candidate.call?.direction !== "incoming" ||
              candidate.call.state !== "ringing"
            )
              continue;
            try {
              await flow!.accept(settings.incomingPrincipalId, id, id);
            } catch (error) {
              if (
                !(error instanceof IvyError) ||
                !["phone_caller_refused", "phone_call_busy"].includes(
                  error.code,
                )
              )
                throw error;
            }
          }
        }
        await flow!.observe(observed);
        if (
          !journal.currentCalls().length &&
          Date.now() - lastVoiceCheck >= 60_000
        )
          checkVoice();
        // Rotate only the owned SIP process, between calls, before its bounded receipt map fills.
        // Closing the flow first fences admissions that raced with the idle observation.
        if (
          journal.epochOperations >= 4000 &&
          !journal.currentCall() &&
          !observed.call
        ) {
          rotating = true;
          try {
            await flow!.close();
            await processOwner!.close();
            if (!stopped) await initialize();
          } catch (error) {
            queueMicrotask(() => {
              void close().catch(() => {
                process.exitCode = 1;
              });
            });
            throw error;
          } finally {
            rotating = false;
          }
          return;
        }
      })()
        .catch((error) => {
          diagnostics.pollFailed(IvyError.from(error).code);
        })
        .finally(() => {
          active = null;
          if (!stopped) timer = setTimeout(tick, settings.pollMs);
        });
    };
    service.start();
    tick();
    return {
      service,
      get bridge() {
        return bridge;
      },
      journal,
      close,
    };
  } catch (error) {
    await close();
    throw error;
  }
}
if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  try {
    const bridge = await startPhoneBridge(configurationPath());
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
        message: "PhoneBridge failed to start.",
      }) + "\n",
    );
    process.exitCode = 1;
  }
}
