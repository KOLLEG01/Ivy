import { computed, watch } from "vue";
import { useRemote } from "@ivy/ui";
import { nativeRead } from "../../../packages/ui-client/src/native";
import { client, readDocument, tr } from "./runtime";
import type { TaskBoard } from "./runtime";
import { canonical } from '../../../packages/sdk/src/client.js';
import type { NativeRouteOwner } from '../../../packages/ui-client/src/native-route';

export function useExecutionDefaults(
  workspace: () => TaskBoard.WorkspaceInfo,
  task: () => TaskBoard.Task | undefined,
) {
  const selection = () => JSON.stringify([task()?.lastRun, task()?.primaryResourceRef]);
  const current = useRemote(
    async (signal) => {
      const value = task(),
        scope = workspace(), selected = selection();
      if (value?.lastRun && value.primaryResourceRef) {
        const run = (
          await readDocument("run", value.lastRun, scope.rootObjectId, signal)
        ).value;
        if (canonical(run.primaryResourceRef) !== canonical(value.primaryResourceRef) || run.target.serviceNodeId !== value.primaryResourceRef.serviceNodeId ||
            (run.target.backend?.backendId ?? 'codex') !== value.primaryResourceRef.namespace)
          throw new Error('The retained execution belongs to another conversation or account.');
        const defaults: TaskBoard.ExecutionDefaults = {
          executionRequirement: { kind: "host", hostId: run.target.hostId },
          nativeOptions: {
            ...(run.target.backend ? { backend: run.target.backend } : {}),
            model:
              typeof run.plan.turnStart.model === "string"
                ? run.plan.turnStart.model
                : null,
            reasoningEffort:
              typeof run.plan.turnStart.effort === "string"
                ? run.plan.turnStart.effort
                : null,
            serviceTier:
              run.plan.turnStart.serviceTier === "fast" ||
              run.plan.turnStart.serviceTier === "flex"
                ? run.plan.turnStart.serviceTier
                : "standard",
          },
        } satisfies TaskBoard.ExecutionDefaults;
        return { defaults, selection: selected, owner: { ...value.primaryResourceRef, ...(run.target.backend ? { backend: run.target.backend } : {}) } as NativeRouteOwner };
      }
      const defaults = (
        (await nativeRead(
          client,
          scope.serviceNodeId,
          "task-board.configuration",
          {},
          signal,
        )) as TaskBoard.ConfigurationView
      ).configuration.defaults;
      return { defaults, selection: selected, owner: null };
    },
    15000,
    ["objects/task-board/configuration", "objects/task-board/run"],
  );
  watch(
    () => JSON.stringify([task()?.lastRun, task()?.primaryResourceRef]),
    () => void current.refresh(),
  );
  const label = computed(() =>
    task()?.lastRun && task()?.primaryResourceRef
      ? tr("Letzte Ausführung", "Previous execution")
      : tr("TaskBoard-Standard", "TaskBoard default"),
  );
  const defaults = computed(() => current.value.value?.selection === selection() ? current.value.value.defaults : null);
  const owner = computed(() => current.value.value?.selection === selection() ? current.value.value.owner : null);
  return { current, defaults, owner, label };
}
