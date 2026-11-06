import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import type { Ref } from "vue";
import { canonical, IvyError, newOperationId } from "../../sdk/src/client.js";
import { confirmsAbsence, record } from "./native.js";
import { backendRead, nativeMethod, sameBackend } from './native-backends.js';
import { backendArguments } from '../../contracts/src/native-backend.js';
import { validateAgent } from '../../contracts/src/agent-validation.js';
import { hashJson } from './hash.js';
import { checkNativeActionReceipt } from './native-action-evidence.js';
import type { NativeActionIdentity, NativeActionCheckpoint } from './native-action-evidence.js';
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
  backend?: Agent.BackendIdentity;
  original?: NativeActionIdentity;
  checkpoint?: NativeActionCheckpoint;
  dispatchUncertain?: boolean;
  callHash?: string;
}
/** UI bookkeeping reads native parameters without changing the retained transport envelope. */
export const nativeActionMethod = (value: SavedNativeAction | null | undefined): string => value?.original?.method ?? value?.call.qualifiedName.split('.').slice(1).join('.') ?? '';
export const nativeActionParams = (value: SavedNativeAction | null | undefined) => record(value?.call.qualifiedName === 'agent.invoke' ? record(value.call.arguments).params : value?.call.arguments);
export function useNativeAction(client: RpcClient, key: string, backend: () => Agent.BackendIdentity | undefined = () => undefined) {
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
      if (value.backend) {
        validateAgent('BackendIdentity', value.backend);
        if (!sameBackend(record(value.call.arguments).backend as Agent.BackendIdentity | undefined, value.backend) && !value.secret)
          throw new Error('The saved action differs from its original account.');
        if (value.original && (!sameBackend(value.original.backend, value.backend) || value.original.serviceNodeId !== value.call.serviceNodeId))
          throw new Error('The saved receipt identity differs from the original route.');
        if (['agent.invoke', 'agent.answer'].includes(value.call.qualifiedName) && !value.original)
          throw new Error('The original receipt identity is missing.');
      }
      if (
        !value.callerPrincipalId ||
        value.call.expectedCallerPrincipalId !== value.callerPrincipalId
      ) {
        value.phase = "unknown";
        value.detail =
          "The original caller is unproven. This retained identity cannot be reassigned or retried.";
      }
      saved.value = value;
      if (['unknown', 'pending'].includes(value.phase)) value.dispatchUncertain = true;
    }
  } catch {
    storageBlocked = true;
    error.value =
      "The retained operation could not be read. Browser storage must be available before a new action.";
  }
  let secretCall: Wire.ToolCall | null = null;
  const observe = async (value: SavedNativeAction, outcome: Agent.Operation) => {
    if (value.original) value.checkpoint = await checkNativeActionReceipt(outcome, value.original, value.callerPrincipalId, value.operationId, value.checkpoint);
    else if (outcome.callerPrincipalId !== value.callerPrincipalId || outcome.operationId !== value.operationId ||
      outcome.serviceNodeId !== value.call.serviceNodeId || !sameBackend(outcome.backend, value.backend))
      throw new IvyError('caller_changed', 'The receipt does not belong to the original account, caller and operation.');
    value.phase = outcome.phase === 'succeeded' ? 'succeeded' : outcome.phase === 'failed' ? 'failed' : outcome.phase === 'outcome_unknown' ? 'unknown' : 'pending';
    const failure = record(record(outcome.reply).error).message;
    value.detail = outcome.phase === 'failed' ? typeof failure === 'string' ? failure : 'The request failed.' : `Owner journal: ${outcome.phase}.`;
    if (outcome.reply && 'result' in outcome.reply) value.result = outcome.reply.result;
  };
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
    if (!sameBackend(value.backend, backend())) {
      error.value = 'Select the original account before retrying this action.';
      return;
    }
    busy.value = true;
    error.value = null;
    let received = false;
    try {
      if (value.callHash && await hashJson(call) !== value.callHash)
        throw new IvyError('native_action_changed', 'The retained call differs from its original native request.');
      if (value.original) {
        const args = record(call.arguments), method = call.qualifiedName === 'agent.invoke' ? args.method : call.qualifiedName;
        const params = call.qualifiedName === 'agent.invoke' ? args.params : { identity: args.identity, reply: args.reply };
        if (!sameBackend(value.backend, value.original.backend) || !sameBackend(args.backend as Agent.BackendIdentity | undefined, value.original.backend) || call.serviceNodeId !== value.original.serviceNodeId ||
          method !== value.original.method || call.qualifiedName === 'agent.invoke' && args.nativeVersion !== value.original.nativeVersion ||
          await hashJson({ method, params }) !== value.original.requestHash)
          throw new IvyError('native_action_changed', 'The retained call differs from its original native request.');
      }
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
      if (!sameBackend(value.backend, backend())) throw new IvyError('native_owner_changed', 'The selected account changed before dispatch.');
      const result = await client.request("tools.call", call);
      received = true;
      if (value.original) {
        await observe(value, result as Agent.Operation);
        persist();
        return;
      }
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
        value.dispatchUncertain || received || e.outcome === "unknown" ||
        ["caller_changed", "caller_unbound", "native_owner_changed", "native_evidence_mismatch", "native_evidence_regressed"].includes(e.code)
          ? "unknown"
          : "failed";
      if (value.phase === 'unknown') value.dispatchUncertain = true;
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
      const originalArgs = JSON.parse(canonical(args)) as Wire.Json;
      binding = { ...binding, definition: { ...binding.definition },
        ...(binding.resourceRef ? { resourceRef: JSON.parse(canonical(binding.resourceRef)) as Wire.ResourceRef } : {}) };
      const selectedValue = backend(), selected = selectedValue ? { ...selectedValue } : undefined;
      const status = await client.request("system.status", {});
      if (!status.callerPrincipalId)
        throw new IvyError(
          "caller_unbound",
          "Hive did not identify the current caller.",
        );
      const callerPrincipalId = status.callerPrincipalId,
        operationId = await newOperationId(client);
      let original: NativeActionIdentity | undefined, selectedBinding = binding;
      // agent.answer carries the same ID in its management payload and outer native journal envelope.
      let argumentsValue =
        binding.qualifiedName === "agent.answer"
          ? { ...record(originalArgs), operationId }
          : originalArgs;
      if (selected) {
        validateAgent('BackendIdentity', selected);
        const native = binding.definition.nativeMethod;
        if (!native && !binding.qualifiedName.startsWith('agent.'))
          throw new IvyError('native_backend_mismatch', 'The action has no selected native method or Agent envelope.');
        if (native && (binding.qualifiedName !== nativeMethod(selected, native) || binding.definition.namespace !== selected.backendId || binding.definition.name !== native))
          throw new IvyError('native_backend_mismatch', 'The native definition belongs to another backend.');
        const owner = await backendRead(client, { serviceNodeId: binding.serviceNodeId, backend: selected }, 'agent.status', {});
        validateAgent('Status', owner);
        const observed = owner as Agent.Status;
        if (observed.serviceNodeId !== binding.serviceNodeId || !sameBackend(observed.backend, selected) || observed.state !== 'ready' || !observed.epoch ||
          native && (binding.definition.nativeSchemaIdentity !== observed.catalogHash || binding.definition.interfaceVersion !== observed.nativeVersion))
          throw new IvyError('native_owner_changed', 'The selected native owner or catalog changed before this action.');
        const method = native ?? binding.qualifiedName, params = native ? originalArgs : { identity: record(originalArgs).identity!, reply: record(originalArgs).reply! };
        if (native || binding.qualifiedName === 'agent.answer') original = { backend: selected, serviceNodeId: observed.serviceNodeId,
          nativeVersion: observed.nativeVersion, nativeExecutableHash: observed.nativeExecutableHash, catalogHash: observed.catalogHash,
          method, requestHash: await hashJson({ method, params }) };
        if (native) {
          selectedBinding = await discover(client, 'agent.invoke', { serviceNodeId: binding.serviceNodeId });
          if (selectedBinding.serviceNodeId !== binding.serviceNodeId || selectedBinding.qualifiedName !== 'agent.invoke')
            throw new IvyError('native_owner_changed', 'The Agent envelope belongs to another service.');
          argumentsValue = { operationId, nativeVersion: observed.nativeVersion, method: native, params: originalArgs, expectedDefinitionHash: binding.definitionHash };
        }
        argumentsValue = backendArguments(argumentsValue, selected);
        if (!sameBackend(selected, backend())) throw new IvyError('native_owner_changed', 'The selected account changed before the action was retained.');
      } else if (binding.definition.nativeMethod) {
        if (binding.definition.namespace !== 'codex')
          throw new IvyError('native_backend_required', 'Select the complete account identity before a native action.');
        const legacy = await backendRead(client, { serviceNodeId: binding.serviceNodeId }, 'agent.status', {});
        validateAgent('Status', legacy);
        const owner = legacy as Agent.Status;
        if (owner.state !== 'ready' || binding.definition.interfaceVersion !== owner.nativeVersion || binding.definition.nativeSchemaIdentity !== owner.catalogHash)
          throw new IvyError('native_owner_changed', 'The original unscoped owner or native definition changed.');
      }
      const prepared: SavedNativeAction = {
        callerPrincipalId,
        operationId,
        label,
        createdAt: new Date().toISOString(),
        phase: "prepared",
        detail: "",
        secret,
        ...(selected ? { backend: selected } : {}),
        ...(original ? { original } : {}),
        call: {
          expectedCallerPrincipalId: callerPrincipalId,
          qualifiedName: selectedBinding.qualifiedName,
          serviceNodeId: selectedBinding.serviceNodeId,
          expectedDefinitionHash: selectedBinding.definitionHash,
          operationId,
          arguments: argumentsValue,
          ...(binding.resourceRef ? { resourceRef: binding.resourceRef } : {}),
        },
      };
      prepared.callHash = await hashJson(prepared.call);
      if (stopped) return;
      saved.value = prepared;
      secretCall = secret ? saved.value.call : null;
      if (secret && record(originalArgs).identity)
        saved.value.inputIdentity = record(originalArgs)
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
        arguments: backendArguments({ operationId: value.operationId }, value.backend),
      })) as Agent.Operation;
      await observe(value, outcome);
      persist();
    } catch (cause) {
      const e = IvyError.from(cause);
      if (value.callerPrincipalId && confirmsAbsence(e, value.call, value.backend)) {
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
      canonical(record(args).identity) !== canonical(value.inputIdentity)
    )
      return;
    secretCall = {
      ...value.call,
      arguments: backendArguments({ ...record(args), operationId: value.operationId }, value.backend),
    };
    if (value.original && await hashJson({ method: value.original.method, params: { identity: record(args).identity!, reply: record(args).reply! } }) !== value.original.requestHash) {
      error.value = 'Re-enter the original response; this answer differs from its retained request.';
      secretCall = null;
      return;
    }
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
    }, error.value ? 5000 : 3000);
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
