import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import type { Ref } from "vue";
import { IvyError, newOperationId } from "../../sdk/src/client.js";
import { confirmsAbsence, record } from "./native";
import { discover } from "../../sdk/src/client.js";
import type {
  Agent,
  BoundTool,
  RpcClient,
  Wire,
} from "../../sdk/src/client.js";

export interface SavedNativeAction {
  callerPrincipalId: string;
  operationId: string;
  label: string;
  createdAt: string;
  call: Wire.ToolCall;
  phase:
    "prepared" | "unknown" | "pending" | "not_found" | "succeeded" | "failed";
  detail: string;
  result?: Wire.Json;
  secret: boolean;
  inputIdentity?: Agent.InputIdentity;
}
export function useNativeAction(client: RpcClient, key: string) {
  const saved = ref(null) as Ref<SavedNativeAction | null>,
    busy = ref(false),
    error = ref<string | null>(null);
  let storageBlocked = false;
  try {
    const raw = sessionStorage.getItem(key);
    if (raw) {
      const value = JSON.parse(raw) as SavedNativeAction;
      if (
        !value.call ||
        !value.operationId ||
        !value.call.serviceNodeId ||
        value.operationId !== value.call.operationId
      )
        throw new Error("Invalid retained operation.");
      if (
        !value.callerPrincipalId ||
        value.call.expectedCallerPrincipalId !== value.callerPrincipalId
      ) {
        value.phase = "unknown";
        value.detail =
          "The original caller is unproven. This retained identity cannot be reassigned or retried.";
      }
      saved.value = value;
    }
  } catch {
    storageBlocked = true;
    error.value =
      "The retained operation could not be read. Browser storage must be available before a new action.";
  }
  let secretCall: Wire.ToolCall | null = null;
  const persist = () => {
    try {
      if (saved.value) {
        const value = structuredClone(
          JSON.parse(JSON.stringify(saved.value)),
        ) as SavedNativeAction;
        if (value.secret) {
          value.call.arguments = null;
          delete value.result;
        }
        if (value.result && JSON.stringify(value.result).length > 524288)
          delete value.result;
        sessionStorage.setItem(key, JSON.stringify(value));
      } else sessionStorage.removeItem(key);
    } catch {
      storageBlocked = true;
      throw new Error(
        "Could not retain the original operation in this tab. The action is paused.",
      );
    }
  };
  const execute = async () => {
    const value = saved.value;
    if (!value) return;
    const call = value.secret ? secretCall : value.call;
    if (!call) {
      error.value =
        "Re-enter the original secret response before retrying; its identity is retained.";
      return;
    }
    busy.value = true;
    error.value = null;
    try {
      value.phase = "unknown";
      value.detail = "Waiting for the original native outcome.";
      persist();
      if (
        !value.callerPrincipalId ||
        call.expectedCallerPrincipalId !== value.callerPrincipalId
      )
        throw new IvyError(
          "caller_unbound",
          "This retained action has no proven original caller; it cannot be dispatched.",
        );
      const result = await client.request("tools.call", call);
      const answer =
        call.qualifiedName === "agent.answer" ? record(result) : null;
      value.phase =
        answer && answer.phase !== "succeeded"
          ? answer.phase === "failed"
            ? "failed"
            : "pending"
          : "succeeded";
      value.result = result;
      value.detail =
        value.phase === "succeeded"
          ? "Native request completed. Live work may continue."
          : "Answer dispatched; awaiting native resolution.";
      persist();
    } catch (cause) {
      const e = IvyError.from(cause);
      value.phase =
        e.outcome === "unknown" ||
        ["caller_changed", "caller_unbound"].includes(e.code)
          ? "unknown"
          : "failed";
      const details = record(e.details), nativeMessage = record(details.nativeError).message;
      value.detail = e.code === "native_error" && details.operationId === value.operationId && typeof nativeMessage === "string"
        ? nativeMessage.slice(0, 4096) : e.message;
      try {
        persist();
      } catch (failure) {
        error.value = String(failure);
      }
    } finally {
      busy.value = false;
    }
  };
  const start = async (
    label: string,
    binding: BoundTool,
    args: Wire.Json,
    secret = false,
  ) => {
    if (
      busy.value ||
      (saved.value && !["failed", "succeeded"].includes(saved.value.phase)) ||
      storageBlocked
    )
      return;
    busy.value = true;
    error.value = null;
    try {
      const status = await client.request("system.status", {});
      if (!status.callerPrincipalId)
        throw new IvyError(
          "caller_unbound",
          "Hive did not identify the current caller.",
        );
      const callerPrincipalId = status.callerPrincipalId,
        operationId = await newOperationId(client);
      // agent.answer carries the same ID in its management payload and outer native journal envelope.
      const argumentsValue =
        binding.qualifiedName === "agent.answer"
          ? { ...record(args), operationId }
          : args;
      saved.value = {
        callerPrincipalId,
        operationId,
        label,
        createdAt: new Date().toISOString(),
        phase: "prepared",
        detail: "",
        secret,
        call: {
          expectedCallerPrincipalId: callerPrincipalId,
          qualifiedName: binding.qualifiedName,
          serviceNodeId: binding.serviceNodeId,
          expectedDefinitionHash: binding.definitionHash,
          operationId,
          arguments: argumentsValue,
          ...(binding.resourceRef ? { resourceRef: binding.resourceRef } : {}),
        },
      };
      secretCall = secret ? saved.value.call : null;
      if (secret && record(args).identity)
        saved.value.inputIdentity = record(args)
          .identity as Agent.InputIdentity;
      persist();
      await execute();
    } catch (cause) {
      error.value = IvyError.from(cause).message;
    } finally {
      busy.value = false;
    }
  };
  const reconcile = async () => {
    if (!saved.value || busy.value) return;
    busy.value = true;
    error.value = null;
    const value = saved.value;
    try {
      if (
        !value.callerPrincipalId ||
        value.call.expectedCallerPrincipalId !== value.callerPrincipalId
      )
        throw new IvyError(
          "caller_unbound",
          "The original caller of this saved action is unknown; automatic reassignment is forbidden.",
        );
      const binding = await discover(client, "agent.operation", {
        serviceNodeId: value.call.serviceNodeId!,
      });
      const outcome = (await client.request("tools.call", {
        qualifiedName: binding.qualifiedName,
        serviceNodeId: binding.serviceNodeId,
        expectedDefinitionHash: binding.definitionHash,
        expectedCallerPrincipalId: value.callerPrincipalId,
        arguments: { operationId: value.operationId },
      })) as Agent.Operation;
      if (
        outcome.callerPrincipalId !== value.callerPrincipalId ||
        outcome.operationId !== value.operationId
      )
        throw new IvyError(
          "caller_changed",
          "The result does not belong to the original caller and operation.",
        );
      value.phase =
        outcome.phase === "succeeded"
          ? "succeeded"
          : outcome.phase === "failed"
            ? "failed"
            : outcome.phase === "outcome_unknown"
              ? "unknown"
              : "pending";
      const failure = record(record(outcome.reply).error).message;
      value.detail = outcome.phase === "failed"
        ? typeof failure === "string" ? failure : "The request failed."
        : `Owner journal: ${outcome.phase}.`;
      if (outcome.reply && "result" in outcome.reply)
        value.result = outcome.reply.result;
      persist();
    } catch (cause) {
      const e = IvyError.from(cause);
      if (value.callerPrincipalId && confirmsAbsence(e, value.call)) {
        value.phase = "not_found";
        value.detail =
          "The selected owner confirms no retained operation for the original caller. You may explicitly retry the original request.";
        persist();
      } else error.value = e.message;
    } finally {
      busy.value = false;
    }
  };
  const retry = async () => {
    if (saved.value?.phase === "not_found" && !busy.value) await execute();
  };
  const reenterSecret = async (args: Wire.Json) => {
    const value = saved.value;
    if (
      !value?.secret ||
      value.phase !== "not_found" ||
      busy.value ||
      !value.inputIdentity ||
      JSON.stringify(record(args).identity) !==
        JSON.stringify(value.inputIdentity)
    )
      return;
    secretCall = {
      ...value.call,
      arguments: { ...record(args), operationId: value.operationId },
    };
    await execute();
  };
  const pending = computed(() =>
    busy.value || !!saved.value && ["prepared", "unknown", "pending"].includes(saved.value.phase),
  );
  // Recover receipts through the original owner's journal; mutations are never replayed here.
  let recoveryTimer: ReturnType<typeof setTimeout> | undefined, stopped = false;
  const scheduleRecovery = () => {
    clearTimeout(recoveryTimer);
    if (stopped || busy.value || !pending.value) return;
    recoveryTimer = setTimeout(async () => {
      if (!document.hidden) await reconcile();
      scheduleRecovery();
    }, error.value ? 15000 : 3000);
  };
  const recoverNow = () => {
    if (!document.hidden && pending.value && !busy.value) void reconcile();
  };
  watch([busy, () => saved.value?.operationId, () => saved.value?.phase], scheduleRecovery);
  onMounted(() => {
    // A retained failure may predate the UI's error display. Read its exact receipt once,
    // without replaying the mutation or polling a terminal outcome.
    if (saved.value?.phase === "failed") void reconcile();
    scheduleRecovery();
    window.addEventListener("online", recoverNow);
    document.addEventListener("visibilitychange", recoverNow);
  });
  onBeforeUnmount(() => {
    stopped = true;
    clearTimeout(recoveryTimer);
    window.removeEventListener("online", recoverNow);
    document.removeEventListener("visibilitychange", recoverNow);
  });
  return {
    saved,
    busy,
    pending,
    error,
    start,
    reconcile,
    retry,
    reenterSecret,
    locked: computed(
      () =>
        busy.value ||
        storageBlocked ||
        (!!saved.value && !["failed", "succeeded"].includes(saved.value.phase)),
    ),
  };
}
