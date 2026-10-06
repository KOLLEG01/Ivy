import { canonical, hashJson } from "../../../packages/sdk/src/node.js";
import { IvyError, requireThat } from "../../../packages/sdk/src/node.js";
import type { Agent, Chat, Wire } from "../../../packages/sdk/src/node.js";
import { validateAgent } from "../../../packages/sdk/src/node.js";
import { nativePath } from "./native-driver.js";
import { ChatMain } from "./chat-main.js";
import { chatNativeCatalog, verifyChatNativeRead } from "./native-evidence.js";
import { nativeThreadState } from "../../../packages/sdk/src/native-observations.js";
import { admittedNativeInput } from "./input-media.js";
import { admittedNoticeInput } from "./notice-input.js";
import { contention } from "./operations.js";
import { mutation } from "./store.js";
import type { Document } from "./store.js";
import { resolveChatPlan, chatTurnOptions } from "./native-plan.js";
import { deriveOperationId } from "../../../packages/sdk/src/node.js";
import { mainInputOrigin, mainTurnContext } from './main-context.js';

const same = (a: unknown, b: unknown) => canonical(a) === canonical(b);
type InputDocument = Document<"chat-bridge/input">;
type Slot = "resume" | "turn";

/** One bounded queue-head delivery pass. Completion/result publication and cancellation are separate. */
export class ChatDelivery {
  constructor(readonly main: ChatMain) {}
  get store() {
    return this.main.store;
  }
  get native() {
    return this.main.native;
  }
  async head(inputId?: string) {
    await this.native.verifyOwner();
    const main = await this.main.queue.main(),
      ticket = main.value.queue[0];
    requireThat(
      !main.value.pendingAction,
      "chat_pending_action",
      "Finish the original Main action before native queue work.",
    );
    if (!ticket) {
      requireThat(
        !inputId,
        "revision_conflict",
        "The selected queue head has already finished.",
      );
      return null;
    }
    requireThat(
      !inputId || inputId === ticket.inputId,
      "revision_conflict",
      "Only the current queue head may deliver native input.",
    );
    const input = await this.store.read("chat-bridge/input", ticket.inputId);
    const operation = await this.main.operations.find(
      input.value.identity.senderPrincipalId,
      input.value.operationId,
    );
    requireThat(
      operation,
      "chat_identity_conflict",
      "Queue delivery requires its original retained action.",
    );
    let request: Chat.SendRequest | Chat.AdmittedNotifyRequest,
      admitted: ReturnType<typeof admittedNoticeInput>;
    if (operation.value.request.action === "notify") {
      request = operation.value.request;
      admitted = admittedNoticeInput(
        this.main.admission,
        input.value.identity.senderPrincipalId,
        request,
        input.value.binding,
      );
    } else {
      requireThat(
        operation.value.request.action === "send",
        "chat_identity_conflict",
        "Only an admitted user message or service handoff may enter the Main queue.",
      );
      request = operation.value.request;
      admitted = {
        ...this.main.admission.send(
          input.value.identity.senderPrincipalId,
          request,
        ),
        payload: request.payload,
      };
    }
    requireThat(
      input.value.workspaceId === this.main.admission.definition.workspaceId &&
        input.value.definitionHash === this.main.admission.definitionHash &&
        same(ticket.identity, admitted.identity) &&
        same(input.value.identity, admitted.identity) &&
        ticket.requestHash === admitted.requestHash &&
        input.value.requestHash === admitted.requestHash &&
        same(input.value.payload, admitted.payload) &&
        same(ticket.binding, input.value.binding) &&
        ticket.sequence === input.value.sequence &&
        ticket.admittedAt === input.value.admittedAt,
      "chat_identity_conflict",
      "Queue delivery must preserve its original admitted payload, caller, binding and sequence.",
    );
    const binding = await this.store.read(
        "chat-bridge/binding",
        input.value.binding,
      ),
      definition = this.main.admission.definition;
    requireThat(
      binding.value.workspaceId === definition.workspaceId &&
        binding.value.definitionHash === this.main.admission.definitionHash &&
        same(binding.value.project, definition.project) &&
        same(binding.value.nativePlan, definition.nativePlan) &&
        binding.value.primary.serviceNodeId ===
          definition.project.serviceNodeId &&
        binding.value.primary.namespace === "codex" &&
        binding.value.primary.kind === "thread",
      "chat_binding_mismatch",
      "The original input must keep its configured native project, plan and owning primary.",
    );
    return { input, binding, request };
  }
  private async inspect(
    input: InputDocument,
    binding: Chat.Binding,
    status: Agent.Status,
    allowUnloaded: boolean,
  ): Promise<boolean> {
    const expected = {
      serviceNodeId: binding.primary.serviceNodeId,
      callerPrincipalId: this.main.admission.definition.principalId,
      nativeVersion: binding.nativePlan.nativeVersion,
      epoch: status.epoch!,
      method: "thread/read",
      params: { threadId: binding.primary.nativeId, includeTurns: false },
    };
    const observed = await this.native.management("read", {
      nativeVersion: expected.nativeVersion,
      method: expected.method,
      params: expected.params,
    });
    verifyChatNativeRead(
      observed,
      expected,
      binding.nativePlan.catalogSourceHash,
    );
    if (
      allowUnloaded &&
      "error" in observed.reply &&
      observed.reply.error.code === -32600 &&
      observed.reply.error.message ===
        `thread not loaded: ${binding.primary.nativeId}`
    )
      return false;
    requireThat(
      "result" in observed.reply,
      "chat_native_unavailable",
      "Native input needs an original successful primary observation.",
    );
    const { thread, state, canStartTurn } = nativeThreadState(
      chatNativeCatalog(expected.nativeVersion, {
        sourceHash: binding.nativePlan.catalogSourceHash,
      }).contract,
      observed.reply.result,
    );
    requireThat(
      thread["id"] === binding.primary.nativeId &&
        thread["ephemeral"] === false &&
        typeof thread["cwd"] === "string" &&
        nativePath(thread["cwd"]) ===
          nativePath(
            (await resolveChatPlan(this.store, binding.nativePlan)).threadStart[
              "cwd"
            ] as string,
          ),
      "chat_native_mismatch",
      "Input must target its original persistent native Main and cwd.",
    );
    requireThat(
      canStartTurn ||
        (allowUnloaded && ["idle", "systemError", "notLoaded"].includes(state)),
      "chat_native_busy",
      "Current native activity or unavailable direct input blocks another Chat turn.",
    );
    // Current availability is a transient decision, not a workflow result. The native
    // action receipt owns recovery; no consumer needs a stored copy of this read.
    return canStartTurn;
  }
  private async attach(
    input: InputDocument,
    slot: Slot,
    call: Document<"chat-bridge/native-call">,
  ): Promise<void> {
    const current = await this.head(input.pin.objectId);
    requireThat(
      current,
      "revision_conflict",
      "The original queue head must remain current.",
    );
    requireThat(
      same(current.input.pin, input.pin) &&
        !current.input.value.cancellation &&
        !current.input.value.result &&
        !current.input.value.turnId,
      "revision_conflict",
      "Native delivery requires its unchanged unfinished input.",
    );
    const previous = input.value.nativeCalls[slot];
    requireThat(
      !previous ||
        previous.objectId === call.pin.objectId ||
        (slot === "resume" && same(call.value.predecessor, previous)),
      "chat_native_mismatch",
      "Only a retained resume predecessor may replace an attached input call.",
    );
    if (same(previous, call.pin)) return;
    let error: Chat.Error | null = null;
    const failed = call.value.state === "failed",
      unknown = call.value.state === "outcome_unknown";
    if (failed || unknown)
      error = {
        code: call.value.code!,
        outcome:
          failed && call.value.epoch === null ? "not_executed" : "unknown",
        detail: "The original native input call " + call.value.state + ".",
      };
    let turnId: string | null = null;
    if (slot === "turn" && call.value.state === "succeeded") {
      requireThat(
        call.value.evidence,
        "chat_native_mismatch",
        "Native turn delivery needs its saved original receipt.",
      );
      const original = await this.native.evidence.operation(
        input.pin.objectId,
        call.value,
        call.value.evidence,
      );
      requireThat(
        original.reply && "result" in original.reply,
        "chat_native_mismatch",
        "The original native turn needs its successful reply.",
      );
      const turn = (original.reply.result as Record<string, Wire.Json>)[
        "turn"
      ] as Record<string, Wire.Json>;
      requireThat(
        typeof turn["id"] === "string",
        "chat_native_mismatch",
        "The native reply must name its original turn.",
      );
      turnId = turn["id"];
    }
    const value: Chat.Input = {
      ...input.value,
      nativeCalls: { ...input.value.nativeCalls, [slot]: call.pin },
      turnId,
      epoch: call.value.epoch,
      state: failed
        ? "failed"
        : unknown
          ? "outcome_unknown"
          : turnId
            ? "running"
            : slot === "turn"
              ? "delivering"
              : "preparing",
      error,
      finishedAt: failed ? call.value.updatedAt : null,
      updatedAt: call.value.updatedAt,
    };
    await this.store.write(
      "chat-bridge/input",
      value,
      mutation(
        input.pin.objectId,
        "native:" + slot + ":" + input.pin.revision + ":" + hashJson(value),
      ),
      { objectId: input.pin.objectId, expectedRevision: input.pin.revision },
    );
  }
  private async resume(
    input: InputDocument,
    binding: Chat.Binding,
    status: Agent.Status,
  ): Promise<void> {
    const previous = input.value.nativeCalls.resume
      ? await this.native.get(
          input.pin.objectId,
          input.value.nativeCalls.resume,
        )
      : null;
    const reattach = previous?.value.state === "succeeded";
    if (previous && !reattach) return this.advance(input, binding, "resume");
    const loaded = await this.inspect(input, binding, status, true);
    if (reattach && previous.value.epoch === status.epoch && loaded)
      return this.advance(input, binding, "resume");
    const origin = reattach
      ? { predecessor: previous.pin, preparedEpoch: status.epoch }
      : { predecessor: null, preparedEpoch: null };
    const plan = await resolveChatPlan(this.store, binding.nativePlan),
      request: Chat.NativeRequest = {
        nativeVersion: plan.nativeVersion,
        catalogSourceHash: plan.catalogSourceHash,
        method: "thread/resume",
        params: { ...plan.threadResume, threadId: binding.primary.nativeId },
      } as Chat.NativeRequest;
    const operationId = deriveOperationId(
      input.value.operationId,
      "chat:native:resume:" + hashJson(origin),
    );
    const call = await this.native.prepare(
      input.pin.objectId,
      operationId,
      request,
      origin,
      { excludeTurns: true },
    );
    await this.attach(input, "resume", call);
  }
  private async advance(
    input: InputDocument,
    binding: Chat.Binding,
    slot: Slot,
  ): Promise<void> {
    const pin = input.value.nativeCalls[slot];
    requireThat(
      pin,
      "chat_native_mismatch",
      "A native input call must be attached before dispatch.",
    );
    const call = await this.native.advance(
      input.pin.objectId,
      pin,
      async () => {
        const current = await this.head(input.pin.objectId),
          status = await this.native.ready();
        requireThat(
          current &&
            same(current.input.pin, input.pin) &&
            !current.input.value.cancellation &&
            same(current.input.value.nativeCalls[slot], pin),
          "revision_conflict",
          "The same queue head must still authorize its original attached native call.",
        );
        if (slot === "turn") {
          const resume = current.input.value.nativeCalls.resume;
          if (resume) {
            const attached = await this.native.get(input.pin.objectId, resume);
            requireThat(
              attached.value.state === "succeeded" &&
                attached.value.epoch === status.epoch,
              "chat_reattach_required",
              "Main must be attached in the current native epoch before the original turn can start.",
            );
          }
          // A freshly started Main is already loaded but may not have a rollout to resume.
          // Recheck the exact primary immediately before dispatch; an unloaded primary must
          // still go through durable resume, and unknown original turns never reach this guard.
          requireThat(
            await this.inspect(current.input, binding, status, true),
            "chat_reattach_required",
            "Main must accept direct input in the current native epoch.",
          );
        } else {
          await this.inspect(current.input, binding, status, true);
        }
        const final = await this.head(input.pin.objectId);
        requireThat(
          final && same(final.input.pin, input.pin),
          "revision_conflict",
          "Input changed during the last native readiness observation.",
        );
      },
    );
    await this.attach(input, slot, call);
  }
  async step(): Promise<void> {
    await this.main.recoverPending();
    const current = await this.head();
    if (!current) return;
    const { input, binding } = current;
    // Persist routing before dispatch so native notifications cannot publish a raw notice.
    if (current.request.action === 'notify') this.store.rememberNoticeInput(input.pin.objectId);
    if (
      input.value.cancellation ||
      input.value.turnId ||
      input.value.finishedAt ||
      input.value.result
    )
      return;
    try {
      if (input.value.nativeCalls.turn) {
        try {
          await this.advance(input, binding.value, "turn");
        } catch (error) {
          if (!(
            error instanceof IvyError && error.code === "chat_reattach_required"
          ))
            throw error;
          // The driver calls this guard only after exact original owner absence. A new resume
          // attaches the same primary; the already retained turn identity is never replaced.
          await this.resume(input, binding.value, await this.native.ready());
        }
        return;
      }
      const status = await this.native.ready();
      const attached = input.value.nativeCalls.resume
        ? await this.native.get(
            input.pin.objectId,
            input.value.nativeCalls.resume,
          )
        : null;
      if (
        attached
          ? attached.value.state !== "succeeded" ||
            attached.value.epoch !== status.epoch ||
            !same(attached.pin, input.value.nativeCalls.resume)
          : !(await this.inspect(input, binding.value, status, true))
      ) {
        await this.resume(input, binding.value, status);
        return;
      }
      const capacity = await this.native.management("frameLimits", {});
      validateAgent("FrameLimits", capacity);
      const limits = capacity as Agent.FrameLimits;
      requireThat(
        limits.serviceNodeId === status.serviceNodeId &&
          limits.epoch === status.epoch &&
          limits.nativeVersion === status.nativeVersion &&
          limits.nativeExecutableHash === status.nativeExecutableHash &&
          limits.catalogHash === status.catalogHash,
        "chat_native_unavailable",
        "Native input requires the current owner's verified frame capacity.",
      );
      const plan = await resolveChatPlan(this.store, binding.value.nativePlan),
        request: Chat.NativeRequest = {
          nativeVersion: plan.nativeVersion,
          catalogSourceHash: plan.catalogSourceHash,
          method: "turn/start",
          params: {
            ...mainTurnContext(chatTurnOptions(plan.turnStart, input.value.payload.turnOptions),
              mainInputOrigin(input.value.identity.senderPrincipalId, current.request)),
            threadId: binding.value.primary.nativeId,
            input:
              current.request.action === "notify"
                ? [{ type: "text", text: input.value.payload.text }]
                : (
                    await admittedNativeInput(
                      this.main.admission,
                      this.store.client,
                      input.value.identity.senderPrincipalId,
                      current.request,
                    )
                  ).input,
            clientUserMessageId: mutation(
              input.pin.objectId,
              "native-user-message",
            ),
          },
        } as Chat.NativeRequest;
      const call = await this.native.prepare(
        input.pin.objectId,
        deriveOperationId(input.value.operationId, "chat:native:turn/start"),
        request,
      );
      await this.attach(input, "turn", call);
    } catch (error) {
      if (
        contention(error) ||
        (error instanceof IvyError && error.code === "chat_native_busy")
      )
        return;
      if (
        error instanceof IvyError &&
        ["chat_image_invalid", "chat_image_mismatch"].includes(error.code) &&
        !input.value.nativeCalls.turn
      ) {
        const unchanged = await this.head(input.pin.objectId);
        requireThat(
          unchanged && same(unchanged.input.pin, input.pin),
          "revision_conflict",
          "An image refusal must still own its unchanged undispatched input.",
        );
        const at = new Date().toISOString(),
          value: Chat.Input = {
            ...input.value,
            state: "failed",
            finishedAt: at,
            updatedAt: at,
            error: {
              code: error.code,
              outcome: "not_executed",
              detail: error.message.slice(0, 2048),
            },
          };
        await this.store.write(
          "chat-bridge/input",
          value,
          mutation(
            input.pin.objectId,
            "refuse-image:" + input.pin.revision + ":" + hashJson(value),
          ),
          {
            objectId: input.pin.objectId,
            expectedRevision: input.pin.revision,
          },
        );
        return;
      }
      throw error;
    }
  }
}
