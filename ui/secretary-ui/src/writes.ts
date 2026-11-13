import { computed, ref, shallowRef } from "vue";
import type { Secretary, Wire } from "../../../packages/sdk/src/client.js";
import {
  IvyError,
  newOperationId,
  serviceTools,
} from "../../../packages/sdk/src/client.js";
import { RetainedRequests } from "../../../packages/ui-client/src/retained-requests";
import { hashJson } from "../../../packages/ui-client/src/hash";
import { base, client, tr } from "./runtime";
import type { Assignment, Pin, SecretaryConfiguration } from "./types";

export type Write = {
  operationId: string;
  callerPrincipalId: string;
  scope: Secretary.Scope;
  kind: "assignment" | "configuration";
  pin: Pin | null;
  value: Assignment | SecretaryConfiguration;
  requestHash: string;
  uncertain: boolean;
};
const argumentsFor = (write: Write) => ({
  operationId: write.operationId,
  [write.kind]: write.pin,
  value: write.value,
});

export function useWrites(node: string, root: string) {
  const storage = new RetainedRequests<Write>(
    "ivy:secretary:writes:" + JSON.stringify([base.href, node, root]),
  );
  // The SDK's read path clones parameters. Keep retained originals as plain JSON.
  const pending = shallowRef<Record<string, Write>>({});
  const error = ref("");
  const active = ref<Record<string, boolean>>({});
  const tools = serviceTools(client, node, [
    { namespace: "secretary", interfaceVersion: "1.0.0" },
  ]);
  const reload = () => {
    pending.value = storage.read();
  };
  try {
    reload();
  } catch (cause) {
    error.value = String(cause);
  }
  const identity = async () => {
    const [caller, binding] = await Promise.all([
      client.request("system.status", {}),
      tools.call("secretary.binding", {}) as unknown as Promise<{
        expectedScope: Secretary.Scope;
      }>,
    ]);
    if (binding.expectedScope.rootObjectId !== root)
      throw new Error(
        tr(
          "Der Secretary-Arbeitsbereich wurde geändert.",
          "The Secretary workspace changed.",
        ),
      );
    return {
      callerPrincipalId: caller.callerPrincipalId,
      scope: binding.expectedScope,
    };
  };
  const prepare = async (
    key: string,
    kind: Write["kind"],
    pin: Pin | null,
    value: Write["value"],
  ) => {
    reload();
    if (pending.value[key]) return pending.value[key]!;
    const owner = await identity();
    const write: Write = {
      ...owner,
      operationId: await newOperationId(client),
      kind,
      pin,
      value,
      requestHash: "",
      uncertain: false,
    };
    write.requestHash = await hashJson(argumentsFor(write));
    storage.retain(key, write);
    reload();
    return write;
  };
  const execute = async (
    key: string,
  ): Promise<{ pin: Pin; write: Write } | null> => {
    if (active.value[key]) return null;
    active.value[key] = true;
    error.value = "";
    let write: Write | undefined;
    try {
      reload();
      write = pending.value[key];
      if (!write) throw new Error("The original request is unavailable.");
      const current = await identity();
      if (
        current.callerPrincipalId !== write.callerPrincipalId ||
        current.scope.secretaryId !== write.scope.secretaryId ||
        current.scope.rootObjectId !== write.scope.rootObjectId ||
        (await hashJson(argumentsFor(write))) !== write.requestHash
      )
        throw new Error(
          tr(
            "Der ursprüngliche Aufrufer oder Auftrag stimmt nicht mehr überein. Der Vorgang bleibt gespeichert.",
            "The original caller or request no longer matches. The original is retained.",
          ),
        );
      const options = { expectedCallerPrincipalId: write.callerPrincipalId };
      let pin: Pin | null = null;
      if (write.uncertain) {
        try {
          const outcome = (await tools.call(
            "secretary.operationRead",
            {
              expectedScope: write.scope,
              operationId: write.operationId,
            },
            undefined,
            options,
          )) as unknown as Secretary.OperationOutcome;
          const action =
            write.kind === "configuration"
              ? "configure_execution"
              : write.pin
                ? "update_assignment"
                : "create_assignment";
          if (
            outcome.operationId !== write.operationId ||
            outcome.action !== action ||
            (outcome.effect &&
              write.pin &&
              outcome.effect.objectId !== write.pin.objectId)
          )
            throw new Error("The receipt differs from the original operation.");
          if (outcome.phase === "failed") {
            storage.complete(key, write.operationId);
            reload();
            throw new Error(
              outcome.errorCode ?? "The original request failed.",
            );
          }
          if (outcome.phase === "succeeded") {
            if (!outcome.effect)
              throw new Error("The confirmed operation has no saved revision.");
            pin = outcome.effect;
          }
        } catch (cause) {
          // Absence alone proves no outcome. Only the same caller/ID/payload may
          // be submitted to Secretary's durable, idempotent workflow again.
          if (!(cause instanceof IvyError) || cause.code !== "not_found")
            throw cause;
        }
      }
      if (!pin) {
        const replay = write.uncertain;
        write.uncertain = true;
        storage.retain(key, write);
        reload();
        try {
          pin = (await tools.call(
            write.kind === "assignment"
              ? "secretary.saveAssignment"
              : "secretary.saveConfiguration",
            argumentsFor(write) as unknown as Wire.Json,
            write.operationId,
            options,
          )) as unknown as Pin;
        } catch (cause) {
          if (
            !replay &&
            cause instanceof IvyError &&
            cause.outcome === "not_executed"
          ) {
            storage.complete(key, write.operationId);
            reload();
          }
          throw cause;
        }
      }
      // Acceptance stands even if releasing browser bookkeeping fails.
      try {
        storage.complete(key, write.operationId);
        reload();
      } catch (cause) {
        error.value = String(cause);
      }
      return { pin, write };
    } catch (cause) {
      error.value =
        (cause instanceof Error ? cause.message : String(cause)) +
        (write && pending.value[key]
          ? tr(
              " Vorgang bleibt gespeichert; erneut prüfen.",
              " Original retained; check again.",
            )
          : "");
      return null;
    } finally {
      active.value[key] = false;
    }
  };
  return {
    pending,
    active,
    error,
    prepare,
    execute,
    unresolved: computed(() => Object.entries(pending.value)),
  };
}
