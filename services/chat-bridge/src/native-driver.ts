import { posix, win32 } from "node:path";
import { canonical, hashJson } from "../../../packages/sdk/src/node.js";
import { connectionOwner } from "../../../packages/sdk/src/connection-owner.js";
import { IvyError, requireThat } from "../../../packages/sdk/src/node.js";
import { validateAgent } from "../../../packages/sdk/src/node.js";
import type { Agent, Chat, Wire } from "../../../packages/sdk/src/node.js";
import { nativeServiceTools } from "../../../packages/sdk/src/client.js";
import { NativeOwner } from "../../../packages/sdk/src/native-owner.js";
import { reconcileNativeOperation } from "../../../packages/sdk/src/native-operation.js";
import { validateNativeStatus } from "../../../packages/sdk/src/native-evidence.js";
import { ChatAdmission } from "./admission.js";
import {
  ChatNativeEvidence,
  chatNativeCatalog,
  verifyChatNativeRequest,
} from "./native-evidence.js";
import { contention } from "./operations.js";
import { ChatStore, mutation } from "./store.js";
import type { Document } from "./store.js";
import { resolveChatPlan, validateChatNativeDraft } from "./native-plan.js";

type CallDocument = Document<"chat-bridge/native-call">;
const same = (a: unknown, b: unknown) => canonical(a) === canonical(b);
const methods = {
  "thread/start": "threadStart",
  "thread/resume": "threadResume",
  "turn/start": "turnStart",
  "turn/interrupt": "turnInterrupt",
} as const;
const immutable = ({
  state: _state,
  epoch: _epoch,
  evidence: _evidence,
  code: _code,
  updatedAt: _at,
  ...value
}: Chat.NativeCall) => value;
const terminal = (state: Chat.NativeCall["state"]) =>
  ["succeeded", "failed", "outcome_unknown"].includes(state);
const callName = (id: string) => "Chat native call " + id;

/** Compare owner paths without resolving them against the ChatBridge host's filesystem. */
export function nativePath(value: string): string {
  const windows = /^[a-z]:[\\/]|^\\\\/i.test(value),
    paths = windows ? win32 : posix;
  requireThat(
    paths.isAbsolute(value),
    "chat_project_mismatch",
    "Main requires an explicit absolute native working directory.",
  );
  const normalized = paths.normalize(value).replace(/[\\/]+$/, "") || "/";
  return windows ? normalized.toLowerCase().replace(/\\/g, "/") : normalized;
}

/** Durable native boundary. The caller must additionally guard the current Main/queue claim. */
export class ChatNativeDriver {
  readonly evidence: ChatNativeEvidence;
  constructor(
    readonly store: ChatStore,
    readonly admission: ChatAdmission,
    readonly owner: { serviceNodeId: string; generation: number },
  ) {
    this.evidence = new ChatNativeEvidence(store, admission.definition);
  }
  async verifyOwner(): Promise<void> {
    const node = await connectionOwner(
      this.store.client,
      this.owner.serviceNodeId,
    );
    requireThat(
      node.principalId === this.admission.definition.principalId &&
        node.serviceName === "chat-bridge" &&
        node.connected &&
        node.synced,
      "chat_principal_mismatch",
      "Chat recovery requires its configured principal and a live synchronized service connection.",
    );
  }
  async management(name: string, args: Wire.Json): Promise<Wire.Json> {
    return this.nativeOwner().management(name, args);
  }
  private nativeOwner(): NativeOwner {
    const definition = this.admission.definition,
      source = chatNativeCatalog(definition.nativePlan.nativeVersion, {
        sourceHash: definition.nativePlan.catalogSourceHash,
      });
    return new NativeOwner(this.store.client, definition.principalId, {
      serviceNodeId: definition.project.serviceNodeId,
      nativeVersion: source.catalog.version,
      nativeExecutableHash: source.catalog.nativeExecutableHash,
      catalogHash: source.catalogHash,
    });
  }
  async ready(): Promise<Agent.Status> {
    await this.verifyOwner();
    const value = await this.management("status", {});
    validateNativeStatus(
      value,
      this.nativeOwner().target,
      "chat_native_unavailable",
    );
    return value;
  }
  async verifyProject(): Promise<void> {
    await this.ready();
    const definition = this.admission.definition,
      plan = await resolveChatPlan(this.store, definition.nativePlan),
      source = chatNativeCatalog(plan.nativeVersion, {
        sourceHash: plan.catalogSourceHash,
      });
    requireThat(
      source.catalog.sourceHash === plan.catalogSourceHash,
      "chat_native_mismatch",
      "The configured native catalog source must match.",
    );
    for (const [method, key] of Object.entries(methods)) {
      const binding = await nativeServiceTools(
        this.store.client,
        definition.project.serviceNodeId,
      ).binding("codex." + method);
      requireThat(
        binding.serviceNodeId === definition.project.serviceNodeId &&
          binding.definitionHash === plan.definitions[key] &&
          binding.definitionHash === hashJson(source.definitions.get(method)),
        "chat_native_definition_changed",
        "Main needs the exact configured native method definitions.",
      );
    }
    const value = await this.management("projects", {});
    validateAgent("ProjectsResult", value);
    const projects = (value as Agent.ProjectsResult).projects.filter(
      (project) => project.nativeId === definition.project.nativeId,
    );
    requireThat(
      projects.length === 1,
      "chat_project_mismatch",
      "The exact configured project must exist on its native owner.",
    );
    const start = plan.threadStart as Record<string, Wire.Json>;
    requireThat(
      typeof start["cwd"] === "string" && start["ephemeral"] !== true,
      "chat_project_mismatch",
      "Main needs an explicit project cwd and a persistent native thread.",
    );
    const cwd = nativePath(start["cwd"]);
    requireThat(
      projects[0]!.paths.some((root) => {
        const path = nativePath(root);
        return (
          cwd === path || cwd.startsWith(path.endsWith("/") ? path : path + "/")
        );
      }),
      "chat_project_mismatch",
      "Main cwd must belong to the exact configured native project.",
    );
    for (const template of [plan.threadResume, plan.turnStart]) {
      const path = (template as Record<string, Wire.Json>)["cwd"];
      requireThat(
        path == null || (typeof path === "string" && nativePath(path) === cwd),
        "chat_project_mismatch",
        "The native plan cannot switch Main to another cwd.",
      );
    }
  }
  async get(
    parentId: string,
    reference: Chat.ObjectPin | string,
  ): Promise<CallDocument> {
    const attached = await this.store.read(
      "chat-bridge/native-call",
      reference,
      parentId,
    );
    const current =
      typeof reference === "string"
        ? attached
        : await this.store.read(
            "chat-bridge/native-call",
            attached.pin.objectId,
            parentId,
          );
    requireThat(
      current.metadata.name === callName(current.value.operationId) &&
        same(immutable(attached.value), immutable(current.value)),
      "chat_native_mismatch",
      "The retained native call cannot change its original request, preparation or identity.",
    );
    if (current.value.evidence) {
      const original = await this.evidence.operation(
        parentId,
        current.value,
        current.value.evidence,
      );
      requireThat(
        original.phase === current.value.state &&
          original.epoch === current.value.epoch &&
          original.code === current.value.code &&
          original.updatedAt === current.value.updatedAt,
        "chat_native_mismatch",
        "The native call must agree with its retained original owner receipt.",
      );
    } else {
      await this.evidence.request(parentId, current.value);
      requireThat(
        current.value.state === "prepared",
        "chat_native_mismatch",
        "An observed native call needs its original owner evidence.",
      );
    }
    return current;
  }
  async prepare(
    parentId: string,
    operationId: string,
    original: Chat.NativeRequest,
    origin: {
      predecessor: Chat.ObjectPin | null;
      preparedEpoch: string | null;
    } = { predecessor: null, preparedEpoch: null },
    responseProjection?: { excludeTurns: true },
  ): Promise<CallDocument> {
    await this.verifyOwner();
    validateChatNativeDraft(original);
    const request = structuredClone(original),
      intent = structuredClone(origin),
      definition = this.admission.definition;
    requireThat(request.method !== 'turn/steer', 'chat_native_mismatch', 'ChatBridge prepares only its retained plan methods.');
    const existing = await this.store.named(
      "chat-bridge/native-call",
      callName(operationId),
      parentId,
    );
    const matches = async (call: CallDocument) => {
      requireThat(
        call.value.operationId === operationId &&
          same(call.value.predecessor, intent.predecessor) &&
          call.value.preparedEpoch === intent.preparedEpoch &&
          same(await this.evidence.request(parentId, call.value), request),
        "chat_native_mismatch",
        "The same native identity must recover its original request and origin.",
      );
      return call;
    };
    if (existing) {
      const saved = await this.get(parentId, existing.pin);
      if (responseProjection && request.method === 'thread/resume') {
        const retained = await this.evidence.request(parentId, saved.value);
        const params = retained.params as Record<string, Wire.Json>;
        if (Object.hasOwn(params, 'excludeTurns')) (request.params as Record<string, Wire.Json>)['excludeTurns'] = params['excludeTurns']!;
      }
      return matches(saved);
    }
    if (responseProjection && request.method === 'thread/resume') (request.params as Record<string, Wire.Json>)['excludeTurns'] = true;
    const at = new Date().toISOString();
    // Validate before publishing bytes; scope verification in evidence.request below precedes dispatch.
    const value: Chat.NativeCall = {
      schemaVersion: 1,
      operationId,
      serviceNodeId: definition.project.serviceNodeId,
      callerPrincipalId: definition.principalId,
      request: { objectId: parentId, revision: 1 },
      nativeVersion: request.nativeVersion,
      catalogSourceHash: request.catalogSourceHash,
      method: request.method,
      ...intent,
      requestHash: hashJson({ method: request.method, params: request.params }),
      expectedDefinitionHash:
        definition.nativePlan.definitions[methods[request.method]],
      createdAt: at,
      updatedAt: at,
      state: "prepared",
      epoch: null,
      evidence: null,
      code: null,
    };
    await verifyChatNativeRequest(this.store, definition, value, request);
    value.request = await this.evidence.saveRequest(parentId, request);
    await this.evidence.request(parentId, value);
    try {
      const pin = await this.store.write(
        "chat-bridge/native-call",
        value,
        mutation(operationId, "prepare"),
        { create: { parentId, name: callName(operationId) } },
      );
      return this.get(parentId, pin);
    } catch (error) {
      if (!contention(error)) throw error;
      const winner = await this.store.named(
        "chat-bridge/native-call",
        callName(operationId),
        parentId,
      );
      requireThat(
        winner,
        "chat_native_mismatch",
        "The competing original native call must remain available.",
      );
      return matches(await this.get(parentId, winner.pin));
    }
  }
  async prevent(
    parentId: string,
    pin: Chat.ObjectPin,
    guard: () => Promise<void>,
  ): Promise<CallDocument> {
    await this.verifyOwner();
    const call = await this.get(parentId, pin);
    if (!terminal(call.value.state)) {
      const request = await this.evidence.request(parentId, call.value);
      await guard();
      await this.verifyOwner();
      // Prevention and admission race inside the original owner's journal. An already accepted
      // operation wins unchanged; uncertainty is reconciled below without dispatch authority.
      try {
        await this.management("prevent", {
          operationId: call.value.operationId,
          nativeVersion: request.nativeVersion,
          method: request.method,
          params: request.params as Wire.Json,
          expectedDefinitionHash: call.value.expectedDefinitionHash,
        });
      } catch (error) {
        if (!(error instanceof IvyError)) throw error;
      }
    }
    return this.advance(parentId, call.pin, async () => {
      throw new IvyError(
        "chat_native_prevention_unknown",
        "Original prevention has no observed outcome; keep the cancellation intent.",
        "unknown",
      );
    });
  }
  async advance(
    parentId: string,
    pin: Chat.ObjectPin,
    guard: () => Promise<void>,
  ): Promise<CallDocument> {
    await this.verifyOwner();
    const call = await this.get(parentId, pin);
    if (terminal(call.value.state)) return call;
    const owner = this.nativeOwner(),
      request = await this.evidence.request(parentId, call.value);
    const result = await reconcileNativeOperation({
      owner,
      call: {
        operationId: call.value.operationId,
        method: call.value.method,
        params: request.params as Wire.Json,
        definitionHash: call.value.expectedDefinitionHash,
      },
      absenceConflictCode: "chat_native_absence_conflict",
      journal: {
        current: call,
        previous: call.value.evidence
          ? await this.evidence.operation(
              parentId,
              call.value,
              call.value.evidence,
            )
          : null,
        wasObserved: call.value.state !== "prepared",
        retain: async (observed) => {
          const evidence = await this.evidence.saveOperation(
            parentId,
            call.value,
            observed,
          );
          const next: Chat.NativeCall = {
            ...call.value,
            state: observed.phase,
            epoch: observed.epoch,
            evidence,
            code: observed.code,
            updatedAt: observed.updatedAt,
          };
          try {
            const saved = await this.store.write(
              "chat-bridge/native-call",
              next,
              mutation(call.pin.objectId, "observe:" + hashJson(observed)),
              {
                objectId: call.pin.objectId,
                expectedRevision: call.pin.revision,
              },
            );
            return this.get(parentId, saved);
          } catch (error) {
            if (!contention(error)) throw error;
            return this.get(parentId, call.pin.objectId);
          }
        },
      },
      onAbsent: async (original, absence) => {
        const status = await this.ready();
        requireThat(
          absence.epoch === status.epoch &&
            (call.value.method === "thread/resume" ||
              call.value.preparedEpoch === null ||
              call.value.preparedEpoch === status.epoch),
          "chat_native_epoch_changed",
          "Native dispatch must use its confirmed prepared owner epoch.",
        );
        // A still-absent resume may attach its unchanged primary in a later epoch.
        return owner.dispatch(original, async () => {
          await guard();
          await this.verifyOwner();
        });
      },
    });
    requireThat(
      result.kind === "observed",
      "chat_native_outcome_unavailable",
      "The original native effect has no observed receipt; keep its identity for recovery.",
    );
    return result.value;
  }
}
