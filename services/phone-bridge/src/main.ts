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
import {
  PhoneDiagnostics,
  phoneMicroReady,
} from "../../../services/phone-bridge/src/runtime/diagnostics.js";
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
import { PhoneVoiceRetention } from "../../../services/phone-bridge/src/runtime/desktop-voice-retention.js";

export async function ensureDesktopRunning(
  native: Pick<PhoneNativeClient, "observeDesktop" | "launchDesktop">,
  application: { appUserModelId: string; startIfMissing: boolean },
) {
  const desktop = await native.observeDesktop(application.appUserModelId);
  requireThat(
    ["ready", "waiting", "absent"].includes(desktop.state),
    "phone_desktop_unavailable",
    "Configured Desktop observation is unavailable or ambiguous.",
  );
  if (desktop.state !== "absent" || !application.startIfMissing)
    return desktop.state;
  const launched = await native.launchDesktop(application, randomUUID());
  requireThat(
    ["ready", "waiting", "submitted"].includes(launched.phase),
    "phone_desktop_unavailable",
    "Configured Desktop activation did not start.",
  );
  return launched.phase;
}

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
  let maintenance: Promise<void> | null = null,
    desktopWork: Promise<void> | null = null,
    appToolsWork: Promise<void> | null = null,
    lastAppToolsCheck = 0;
  const retention = settings.voiceArchive ? new PhoneVoiceRetention(
    journal, settings.voiceArchive,
    undefined, undefined, undefined,
    (threadId, code) => process.stderr.write(JSON.stringify({ event: 'phone_voice_retention_issue', threadId, code }) + '\n'),
  ) : null;
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
  const checkAppTools = () => {
    if (!settings.voiceArchive || !flow || stopped || appToolsWork) return;
    const selected = flow;
    lastAppToolsCheck = Date.now();
    appToolsWork = selected.prewarmAppTools()
      .then(() => {
        if (selected === flow && diagnostics.appToolsSucceeded())
          process.stdout.write(JSON.stringify({ event: 'phone_app_tools_recovered' }) + '\n');
      }, error => {
        if (selected !== flow || stopped) return;
        const code = IvyError.from(error).code;
        if (diagnostics.appToolsFailed(code))
          process.stderr.write(JSON.stringify({ event: 'phone_app_tools_unavailable', code }) + '\n');
      })
      .finally(() => { appToolsWork = null; });
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
        await Promise.allSettled([maintenance, desktopWork, appToolsWork].filter((work): work is Promise<void> => work !== null));
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
        micro: settings.voiceInput["micro"],
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
      flow = new PhoneFlow(
        new PhoneCallCommands(admission, journal, native),
        native,
        settings,
        () => phoneCredentials(settings.credentialsPath),
        (callId, code) => {
          diagnostics.callFailed(callId, code);
          if (code.startsWith('app_tools_') && diagnostics.appToolsFailed(code))
            process.stderr.write(JSON.stringify({ event: 'phone_app_tools_unavailable', code }) + '\n');
          process.stderr.write(
            JSON.stringify({ event: "phone_call_failed", callId, code }) + "\n",
          );
        },
        undefined,
        logs,
        undefined,
        (callId) => diagnostics.callSucceeded(callId),
      );
      // App Tools may be unavailable during a Desktop update. SIP and the Windows
      // route still need to start; Voice calls will use the same checked pool.
      checkAppTools();
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
        const appToolsIssue = currentDiagnostics.find(item =>
          item.resource !== null &&
          typeof item.resource === "object" &&
          !Array.isArray(item.resource) &&
          item.resource.scope === "app_tools",
        );
        await writeHealth(
          true,
          connection.generation,
          appToolsIssue
            ? `Codex App Tools unavailable (${appToolsIssue.code}); SIP remains active.`
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
            "Local Phone owner remains active during Hive reconnect; Hive tools are unavailable.",
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
        if (observed.micro && !phoneMicroReady(observed.micro.state))
          diagnostics.pollFailed(
            observed.micro.errorCode ?? "phone_micro_not_ready",
          );
        else diagnostics.pollSucceeded();
        // SIP admission is local to this fenced native owner. A Hive reconnect
        // must not consume the provider's ringing window.
        if (!stopped && !rotating && journal.epoch === native.epoch && settings.incomingPrincipalId) {
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
        if (service?.ready && retention && !maintenance)
          maintenance = retention.tick()
            .catch(error => { diagnostics.pollFailed(IvyError.from(error).code); })
            .finally(() => { maintenance = null; });
        if (!journal.currentCalls().length && Date.now() - lastAppToolsCheck >= 60_000)
          checkAppTools();
        if (settings.application.startIfMissing && !desktopWork)
          desktopWork = ensureDesktopRunning(native, settings.application)
            .then(() => undefined, error => { diagnostics.pollFailed(IvyError.from(error).code); })
            .finally(() => { desktopWork = null; });
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
