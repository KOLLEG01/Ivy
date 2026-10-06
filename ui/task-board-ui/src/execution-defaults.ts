import { computed, watch } from "vue";
import { useRemote } from "@ivy/ui";
import { nativeRead } from "../../../packages/ui-client/src/native";
import { client, readDocument, tr } from "./runtime";
import type { TaskBoard } from "./runtime";

export function useExecutionDefaults(
  workspace: () => TaskBoard.WorkspaceInfo,
  task: () => TaskBoard.Task | undefined,
) {
  const current = useRemote(
    async (signal) => {
      const value = task(),
        scope = workspace();
      if (value?.lastRun && value.primaryResourceRef) {
        const run = (
          await readDocument("run", value.lastRun, scope.rootObjectId, signal)
        ).value;
        return {
          executionRequirement: { kind: "host", hostId: run.target.hostId },
          nativeOptions: {
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
      }
      return (
        (await nativeRead(
          client,
          scope.serviceNodeId,
          "task-board.configuration",
          {},
          signal,
        )) as TaskBoard.ConfigurationView
      ).configuration.defaults;
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
  return { current, defaults: current.value, label };
}
