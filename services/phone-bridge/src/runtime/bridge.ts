import { randomUUID } from "node:crypto";
import { IvyError, requireThat } from "../../../../packages/sdk/src/node.js";
import type { Agent, Wire } from "../../../../packages/sdk/src/node.js";
import type { InvocationContext } from "../../../../packages/sdk/src/service.js";
import type { PhoneCall } from "./admission.js";
import { PhoneFlow } from "./flow.js";
import { PhoneNativeClient } from "./native.js";
import { phoneTools, validatePhoneService } from "./registry.js";
import type { PhoneTool } from "./registry.js";
import { audioSetup } from "./audio-setup.js";
import type { PhoneAudioInventory } from "./audio-setup.js";
import type { PhoneVoiceSelection } from "./voice-selection.js";

/** Hive supplies caller identity for audit and operation attribution. Target integrity and retained results stay in PhoneFlow. */
export class PhoneBridge {
  constructor(
    readonly flow: PhoneFlow,
    private readonly native: PhoneNativeClient,
  ) {}
  private original(caller: string, callId: string): PhoneCall {
    const call = this.flow.calls.journal.getCall(callId);
    requireThat(
      call,
      "phone_call_missing",
      "Original Phone admission is unavailable.",
    );
    this.flow.calls.admission.authorize(caller, call, "read");
    return call;
  }
  private async view(caller: string, callId: string) {
    const call = this.original(caller, callId);
    let current =
        this.flow.calls.journal.currentCall(callId)?.callId === callId,
      observation = null;
    if (current) {
      try {
        const status = await this.native.observe(callId);
        current =
          this.flow.calls.journal.currentCall(callId)?.callId === callId;
        if (current && status.call?.id === callId) observation = status.call;
      } catch (error) {
        if (!(error instanceof IvyError)) throw error;
        current =
          this.flow.calls.journal.currentCall(callId)?.callId === callId;
      }
    }
    return { call, current, observation };
  }
  async invoke(
    name: PhoneTool,
    args: Record<string, Wire.Json>,
    context: InvocationContext,
  ): Promise<Wire.Json> {
    context.signal.throwIfAborted();
    const caller = context.callerPrincipalId;
    const tool = phoneTools.find((item) => item.name === name)!;
    validatePhoneService(tool.input, args);
    let result: unknown;
    switch (name) {
      case "probeLoopback": {
        requireThat(
          !this.flow.calls.journal.currentCalls().length,
          "phone_call_busy",
          "Loopback probing requires idle Phone.",
        );
        const reply = await this.native.request("audio.probe", {}, null, 15000);
        requireThat(
          reply.ok,
          "phone_probe_unavailable",
          "Original loopback probe was not completed.",
        );
        result = reply.result;
        break;
      }
      case "audioSetup": {
        const reply = await this.native.request("inventory", {}, null);
        requireThat(
          reply.ok,
          "phone_inventory_unavailable",
          "Native audio inventory is unavailable.",
        );
        validatePhoneService("PhoneInventory", reply.result);
        result = audioSetup(
          reply.result as PhoneAudioInventory,
          this.flow.audioProfiles(),
        );
        break;
      }
      case "codecTest": {
        requireThat(
          !this.flow.calls.journal.currentCalls().length,
          "phone_call_busy",
          "Codec diagnostics require idle Phone.",
        );
        const reply = await this.native.request("codec.test", {}, null);
        requireThat(
          reply.ok,
          "phone_codec_test_unavailable",
          "Native codec diagnostics are unavailable.",
        );
        result = reply.result;
        break;
      }
      case "inventory": {
        const reply = await this.native.request("inventory", {}, null);
        requireThat(
          reply.ok,
          "phone_inventory_unavailable",
          "Native audio inventory is unavailable.",
        );
        result = reply.result;
        break;
      }
      case "reconnect": {
        const id = String(args["operationId"]),
          prior = this.flow.calls.journal.get(id);
        requireThat(
          !prior || prior.intent.method === "registration.reconnect",
          "mutation_conflict",
          "Operation belongs to another command.",
        );
        if (prior) {
          requireThat(
            prior.phase === "result" && prior.receipt?.ok,
            "phone_operation_retained",
            "Original reconnect is unconfirmed; it cannot be repeated.",
          );
          result = prior.receipt.result;
        } else {
          requireThat(
            !this.flow.calls.journal.currentCalls().length,
            "phone_call_busy",
            "Reconnect requires all original calls to finish.",
          );
          const reply = await this.native.request(
            "registration.reconnect",
            {},
            id,
          );
          requireThat(
            reply.ok,
            "phone_reconnect_failed",
            "Original reconnect was not confirmed.",
          );
          result = reply.result;
        }
        break;
      }
      case "calls":
        result = { calls: this.flow.calls.journal.currentCalls() };
        break;
      case "status": {
        const observed = await this.native.observe(),
          { admission, journal } = this.flow.calls;
        const originals = journal.currentCalls(),
          visible: PhoneCall[] = [...originals],
          pending = [];
        for (const id of observed.callIds ??
          (observed.call ? [observed.call.id] : [])) {
          if (journal.getCall(id)) continue;
          const candidate =
            observed.call?.id === id
              ? observed.call
              : (await this.native.observe(id)).call;
          if (
            candidate?.direction !== "incoming" ||
            candidate.state !== "ringing"
          )
            continue;
          try {
            admission.incoming(
              this.native.epoch,
              randomUUID(),
              caller,
              candidate,
            );
            pending.push(candidate);
          } catch (error) {
            if (
              !(error instanceof IvyError) ||
              error.code !== "phone_caller_refused"
            )
              throw error;
          }
        }
        const voiceCall = visible.find(
          (call) => this.flow.callRoute(call) === "voice",
        );
        let voiceModels: unknown[] = [], voiceModelsError: string | null = null;
        try { voiceModels = await this.flow.voiceModelCatalog(); }
        catch (error) { voiceModelsError = IvyError.from(error).code; }
        result = {
          epoch: this.native.epoch,
          registration: observed.registration,
          micro: observed.micro ?? null,
          configuration: this.flow.configuration(),
          busy: !!originals.length || !!observed.call,
          recipients: admission.definition.recipients.map((item) => item.id),
          voiceSelection: voiceCall
            ? this.flow.voiceSelection(voiceCall.callId)
            : null,
          voiceModels,
          voiceModelsError,
          call: visible[0] ?? null,
          incoming: pending[0] ?? null,
          calls: visible,
          incomingCalls: pending,
        };
        break;
      }
      case "request":
        result = await this.flow.request(
          caller,
          String(args["operationId"]),
          args["recipientId"] === undefined
            ? null
            : String(args["recipientId"]),
          args["route"] as "voice" | "windows" | undefined,
          args["destination"] as string | undefined,
          args["voicePrompt"] as string | undefined,
        );
        break;
      case "voiceCall":
        result = await this.flow.request(
          caller,
          String(args["operationId"]),
          String(args["recipientId"]),
          "voice",
          undefined,
          String(args["initialPrompt"]),
        );
        break;
      case "selectVoice":
        result = await this.flow.controlVoice(
          caller,
          String(args["callId"]),
          String(args["operationId"]),
          {
            action: "select",
            selection: {
              model: args["model"] as PhoneVoiceSelection["model"],
              reasoningEffort: args[
                "reasoningEffort"
              ] as PhoneVoiceSelection["reasoningEffort"],
            },
          },
        );
        break;
      case "restartVoice":
        result = await this.flow.controlVoice(
          caller,
          String(args["callId"]),
          String(args["operationId"]),
          { action: "restart" },
        );
        break;
      case "history": {
        result = {
          calls: this.flow.calls.journal.history(
            null,
            Number(args["limit"] ?? 20),
          ),
        };
        break;
      }
      case "screen":
        result = await this.flow.screen(
          caller,
          String(args["operationId"]),
          String(args["recipientId"]),
          {
            announcement: String(args["announcement"]),
            timeoutSeconds: Number(args["timeoutSeconds"] ?? 30),
            repeatCount: Number(args["repeatCount"] ?? 1),
          },
        );
        break;
      case "bridgeScreening":
        result = await this.flow.bridgeScreening(
          caller,
          String(args["callId"]),
        );
        break;
      case "accept":
        result = await this.flow.accept(
          caller,
          String(args["operationId"]),
          String(args["callId"]),
        );
        break;
      case "call":
        result = await this.view(caller, String(args["callId"]));
        break;
      case "voiceInputs":
        result = {
          inputs: this.flow.voiceInputs(caller, String(args["callId"])),
        };
        break;
      case "answerVoiceInput":
        this.flow.answerVoiceInput(
          caller,
          String(args["callId"]),
          args["id"] as Agent.RequestId,
          args["reply"] as Agent.Reply,
        );
        result = { state: "submitted" };
        break;
      case "logs":
        this.original(caller, String(args["callId"]));
        result = {
          entries:
            this.flow.logs?.read(
              String(args["callId"]),
              Number(args["limit"] ?? 20),
            ) ?? [],
        };
        break;
      case "operation": {
        if (args["operationId"] !== undefined) {
          const call = this.original(caller, String(args["callId"]));
          const operation = this.flow.calls.journal.get(
            String(args["operationId"]),
          );
          requireThat(
            !operation || operation.intent.callId === call.callId,
            "mutation_conflict",
            "Operation belongs to another call.",
          );
          result = operation;
        } else
          result = this.flow.calls.read(
            caller,
            String(args["callId"]),
            args["method"] as Parameters<typeof this.flow.calls.read>[2],
            Number(args["generation"] ?? 0),
          );
        break;
      }
      case "hangup":
        await this.flow.hangup(caller, String(args["callId"]));
        result = await this.view(caller, String(args["callId"]));
        break;
    }
    validatePhoneService(tool.output, result);
    context.signal.throwIfAborted();
    return result as Wire.Json;
  }
}
