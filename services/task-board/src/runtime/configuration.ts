import {
  requireThat,
  scalarTypes,
  validateTaskBoard,
} from "../../../../packages/sdk/src/node.js";
import type {
  TaskBoard,
  Operation,
} from "../../../../packages/sdk/src/node.js";
import {
  newOperationId,
  BoundToolClient,
} from "../../../../packages/sdk/src/client.js";
import type { RpcClient } from "../../../../packages/sdk/src/client.js";
import { freshContextRecovery } from "./native-recovery.js";
import { TaskBoardStore } from "./store.js";

export const configurationName = "TaskBoard configuration";
export const initialConfiguration = (): TaskBoard.Configuration => ({
  schemaVersion: 1,
  defaults: {
    executionRequirement: null,
    nativeOptions: {
      model: "gpt-6.1-sol",
      reasoningEffort: "xhigh",
      serviceTier: "standard",
    },
    userContact: "chat",
    useWorktree: false,
    allowParallel: false,
  },
});

export async function configurationView(
  store: TaskBoardStore,
): Promise<TaskBoard.ConfigurationView> {
  const current = await store.named(
    "task-board/configuration",
    configurationName,
  );
  return {
    object: current?.pin ?? null,
    configuration: current?.value ?? initialConfiguration(),
  };
}

export async function checkConfiguration(
  store: TaskBoardStore,
  value: TaskBoard.Configuration,
  serviceNodeId?: string,
): Promise<void> {
  validateTaskBoard("Configuration", value);
  const { executionRequirement: requirement, nativeOptions: options } =
    value.defaults;
  const nodes = [];
  let cursor: string | undefined;
  do {
    const page = await store.client.request("serviceNodes.list", {
      serviceName: "agent-manager",
      limit: 200,
      ...(cursor ? { cursor } : {}),
    });
    nodes.push(...page.items);
    cursor = page.nextCursor ?? undefined;
  } while (cursor);
  requireThat(
    requirement?.kind !== "host" ||
      nodes.some((node) => node.hostId === requirement.hostId),
    "task_board_target_unavailable",
    "Choose a registered AgentManager host.",
  );
  const eligible = nodes.filter(
    (node) =>
      node.connected &&
      node.synced &&
      node.ready &&
      (!serviceNodeId || node.serviceNodeId === serviceNodeId) &&
      (requirement?.kind !== "host" || node.hostId === requirement.hostId),
  );
  // A registered offline host may be selected, but explicit native options need live discovery.
  if (!eligible.length) {
    requireThat(
      !options.model &&
        !options.reasoningEffort &&
        (!options.serviceTier || options.serviceTier === "standard"),
      "task_board_target_unavailable",
      "Native execution options cannot be verified while the selected AgentManager is unavailable. Retry when the host is ready.",
    );
    return;
  }
  for (const node of eligible) {
    if (!(await supportsExecutionOptions(store.client, node, options)))
      continue;
    return;
  }
  requireThat(
    false,
    "task_board_model_unavailable",
    `No ready AgentManager${requirement?.kind === "host" ? " on " + requirement.hostId : ""} supports model=${options.model ?? "default"}, reasoning=${options.reasoningEffort ?? "default"}, speed=${options.serviceTier ?? "standard"}. Choose exact values from the host's model/list catalog.`,
  );
}

export function executionWithDefaults(
  fields: TaskBoard.TaskFields,
  defaults: TaskBoard.ExecutionDefaults,
): TaskBoard.ExecutionDefaults {
  return {
    executionRequirement:
      fields.executionRequirement ?? defaults.executionRequirement,
    nativeOptions: {
      model: fields.nativeOptions?.model ?? defaults.nativeOptions.model,
      reasoningEffort:
        fields.nativeOptions?.reasoningEffort ??
        defaults.nativeOptions.reasoningEffort,
      serviceTier:
        fields.nativeOptions?.serviceTier ??
        defaults.nativeOptions.serviceTier ??
        "standard",
    },
  };
}

export async function effectiveExecution(
  store: TaskBoardStore,
  task: TaskBoard.Task,
): Promise<TaskBoard.ExecutionDefaults> {
  const previous =
    task.lastRun && task.primaryResourceRef
      ? (await store.read("task-board/run", task.lastRun)).value
      : await freshContextRecovery(store, task);
  const defaults: TaskBoard.ExecutionDefaults = previous
    ? {
        executionRequirement: { kind: "host", hostId: previous.target.hostId },
        nativeOptions: {
          model:
            typeof previous.plan.turnStart.model === "string"
              ? previous.plan.turnStart.model
              : null,
          reasoningEffort:
            typeof previous.plan.turnStart.effort === "string"
              ? previous.plan.turnStart.effort
              : null,
          serviceTier:
            previous.plan.turnStart.serviceTier === "fast" ||
            previous.plan.turnStart.serviceTier === "flex"
              ? previous.plan.turnStart.serviceTier
              : "standard",
        },
      }
    : (await configurationView(store)).configuration.defaults;
  return executionWithDefaults(task.fields, defaults);
}

export async function supportsExecutionOptions(
  client: RpcClient,
  node: Pick<Operation.ServiceNode, "serviceNodeId" | "nativeVersion">,
  options: TaskBoard.NativeOptions,
): Promise<boolean> {
  requireThat(
    node.nativeVersion,
    "task_board_target_unavailable",
    "The selected host has no native version.",
  );
  const tools = new BoundToolClient(client, node.serviceNodeId, [
    { namespace: "agent", interfaceVersion: "1.0.0" },
    { namespace: "codex", interfaceVersion: node.nativeVersion },
  ]);
  if (options.model || options.reasoningEffort) {
    const models: {
      model: string;
      id: string;
      isDefault?: boolean;
      supportedReasoningEfforts: { reasoningEffort: string }[];
    }[] = [];
    let next: string | undefined;
    const cursors = new Set<string>();
    do {
      const page = (await tools.call(
        "codex.model/list",
        { limit: 100, includeHidden: true, ...(next ? { cursor: next } : {}) },
        await newOperationId(client),
      )) as unknown as { data: typeof models; nextCursor: string | null };
      models.push(...page.data);
      next = page.nextCursor ?? undefined;
      requireThat(
        models.length <= 1000 && (!next || !cursors.has(next)),
        "task_board_model_unavailable",
        "Model discovery exceeded its complete snapshot limit.",
      );
      if (next) cursors.add(next);
    } while (next);
    const model = options.model
      ? models.find(
          (model) =>
            model.model === options.model || model.id === options.model,
        )
      : models.find((model) => model.isDefault);
    if (
      !model ||
      (options.reasoningEffort &&
        !model.supportedReasoningEfforts.some(
          (effort) => effort.reasoningEffort === options.reasoningEffort,
        ))
    )
      return false;
  }
  if (options.serviceTier && options.serviceTier !== "standard") {
    const bindings = await Promise.all(
      ["thread/start", "turn/start"].map((method) =>
        tools.binding("codex." + method),
      ),
    );
    if (
      bindings.some(
        (binding) =>
          !scalarTypes(binding.definition.inputSchema, "/serviceTier").has(
            "string",
          ),
      )
    )
      return false;
  }
  return true;
}
