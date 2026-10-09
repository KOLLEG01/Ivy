<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  Alert,
  AlertDescription,
  Button,
  Disclosure,
  Field,
  Input,
  Label,
  OptionSelect,
  PageHeader,
  Popover,
  PopoverContent,
  PopoverTrigger,
  Sheet,
  SheetContent,
  SheetDescription,
  SheetTitle,
  useRemote,
} from "@ivy/ui";
import { ListFilter, Plus, Search, SlidersHorizontal, X } from "@lucide/vue";
import { route } from "../../../packages/ui-client/src/runtime";
import { newOperationId } from "../../../packages/sdk/src/client.js";
import {
  and,
  base,
  client,
  readDocument,
  rootWhere,
  taskContractVersions,
  taskStatuses,
} from "./runtime";
import type { Operation, TaskBoard } from "./runtime";
import { useAction } from "./action";
import ActionState from "./ActionState.vue";
import { taskFilter } from "./task-filter";
import {
  boardColumns,
  stateLabel,
  taskContext,
  transitions,
} from "./board";
import type { BoardTransition, Placement, TransitionContext } from "./board";
import BoardColumn from "./BoardColumn.vue";
import TaskDetail from "./TaskDetail.vue";
const props = defineProps<{
  workspace: TaskBoard.WorkspaceInfo;
  query: URLSearchParams;
  available: boolean;
}>();
const search = ref(props.query.get("q") ?? ""),
  status = ref(props.query.get("status") ?? ""),
  control = ref(props.query.get("control") ?? ""),
  host = ref(props.query.get("host") ?? ""),
  category = ref(props.query.get("category") ?? "");
const swimlaneKey =
  "ivy.task-board.swimlane:" +
  base.href +
  ":" +
  props.workspace.serviceNodeId +
  ":" +
  props.workspace.callerPrincipalId;
// The board and the backlog keep separate swimlane preferences.
const storedLane = (key: string) =>
  localStorage.getItem(key) === "category" ? "category" : "none";
const swimlane = ref(storedLane(swimlaneKey)),
  backlogSwimlane = ref(storedLane(swimlaneKey + ":backlog"));
watch(swimlane, (value) => localStorage.setItem(swimlaneKey, value));
watch(backlogSwimlane, (value) =>
  localStorage.setItem(swimlaneKey + ":backlog", value),
);
const hosts = useRemote((signal) =>
  client.request(
    "serviceNodes.list",
    { serviceName: "agent-manager", limit: 200 },
    { signal },
  ), 0, ["services/agent-manager"]);
const filterKey = computed(() =>
  JSON.stringify(
    ["q", "status", "control", "host", "category"].map((key) =>
      props.query.get(key),
    ),
  ),
);
const backlogColumn = { id: "backlog", title: "Backlog", states: ["backlog"] },
  archiveColumn = { id: "archive", title: "Archive", states: [] };
// The archive is fetched only while it is open.
const archiveOpen = ref(false);
const columns = computed(() =>
  boardColumns.filter(
    (column) =>
      !props.query.get("status") ||
      column.states.includes(props.query.get("status")!),
  ),
);
const showBacklog = computed(
  () => !props.query.get("status") || props.query.get("status") === "backlog",
);
const noMatchingTasks = computed(
  () =>
    !!counts.value.value &&
    columns.value.every((column) => counts.value.value![column.id] === 0) &&
    (!showBacklog.value || counts.value.value.backlog === 0),
);
const activeFilters = computed(
  () =>
    ["status", "control", "host", "category"].filter((key) =>
      props.query.get(key),
    ).length,
);
const filtersOpen = ref(false);
const link = (values: Record<string, string> = {}) => {
  const query = new URLSearchParams(props.query);
  for (const [key, value] of Object.entries(values)) {
    if (value) query.set(key, value);
    else query.delete(key);
  }
  return route("tasks", {
    ...Object.fromEntries(query),
    node: props.workspace.serviceNodeId,
  });
};
const counts = useRemote(async (signal) => {
  const seen = new Map<string, string>(),
    cursors = new Set<string>();
  let cursor: string | null = null;
  do {
    const page: Operation.ObjectsQueryResult = await client.request(
      "objects.query",
      {
        contractKey: "task-board/task",
        contractVersions: taskContractVersions,
        where: taskFilter(props.workspace.rootObjectId, props.query),
        select: ["data:/workflowState"],
        orderBy: [{ field: "object.id", direction: "asc" }],
        limit: 200,
        ...(cursor ? { cursor } : {}),
      },
      { signal },
    );
    for (const item of page.items)
      seen.set(item.objectId, String(item.values["data:/workflowState"]));
    cursor = page.nextCursor;
    if (cursor && cursors.has(cursor))
      throw new Error("Task counts could not finish. Refresh the board.");
    if (cursor) cursors.add(cursor);
  } while (cursor);
  return Object.fromEntries(
    [...boardColumns, backlogColumn].map((column) => [
      column.id,
      [...seen.values()].filter((status) => column.states.includes(status))
        .length,
    ]),
  );
}, 30000, ["objects/task-board/task"]);
watch(filterKey, () => {
  counts.value.value = null;
  void counts.refresh();
  search.value = props.query.get("q") ?? "";
  status.value = props.query.get("status") ?? "";
  control.value = props.query.get("control") ?? "";
  host.value = props.query.get("host") ?? "";
  category.value = props.query.get("category") ?? "";
});
const columnRefs = ref<InstanceType<typeof BoardColumn>[]>([]),
  backlogRef = ref<InstanceType<typeof BoardColumn> | null>(null),
  archiveRef = ref<InstanceType<typeof BoardColumn> | null>(null),
  action = useAction(() => props.workspace, "board"),
  error = ref(""),
  preparing = ref(false);
const refresh = () => {
  void counts.refresh();
  for (const column of columnRefs.value) void column.refresh();
  void backlogRef.value?.refresh();
  void archiveRef.value?.refresh();
};
const recovered = () => {
  if (action.saved.value?.phase === "succeeded") error.value = "";
  refresh();
};
watch([swimlane, backlogSwimlane], refresh);
const submit = () => {
  filtersOpen.value = false;
  location.hash = link({
    q: search.value,
    status: status.value,
    control: control.value,
    host: host.value,
    category: category.value,
    id: "",
    panel: "",
  });
};
const selected = computed({
  get: () => !!props.query.get("id"),
  set: (value) => {
    if (!value) location.hash = link({ id: "", panel: "", run: "" });
  },
});
const disabled = computed(
  () => !props.available || preparing.value || action.locked.value,
);
const transition = async (id: string, operation: BoardTransition) => {
  if (disabled.value || props.workspace.role !== "user") return;
  preparing.value = true;
  error.value = "";
  try {
    const current = await readDocument(
      "task",
      id,
      props.workspace.rootObjectId,
    );
    if (
      current.read.object.effectivelyArchived ||
      !transitions(taskContext(current.value)).some(
        (item) => item.action === operation,
      )
    )
      throw new Error(
        "This task changed. Open it to review the available actions.",
      );
    await action.start("Move task to " + operation, {
      action: "transition",
      taskId: id,
      expectedRevision: current.pin.revision,
      workflowState: operation,
      detail: null,
    });
    refresh();
  } catch (cause) {
    error.value =
      cause instanceof Error
        ? cause.message
        : "This transition is unavailable.";
  } finally {
    preparing.value = false;
  }
};
const archiveTasks = async (id?: string) => {
  if (disabled.value || props.workspace.role !== "user") return;
  preparing.value = true;
  error.value = "";
  let archived = 0;
  try {
    const ids: string[] = [];
    if (id) ids.push(id);
    else {
      // Collect the whole filtered Done column before archiving changes its query pages.
      let cursor: string | null = null;
      do {
        const page: Operation.ObjectsQueryResult = await client.request("objects.query", {
          contractKey: "task-board/task",
          contractVersions: taskContractVersions,
          where: and(taskFilter(props.workspace.rootObjectId, props.query), {
            op: "in", field: "data:/workflowState", value: ["done", "cancelled"],
          }),
          orderBy: [{ field: "object.id", direction: "asc" }],
          limit: 200,
          ...(cursor ? { cursor } : {}),
        });
        ids.push(...page.items.map((item) => item.objectId));
        cursor = page.nextCursor;
      } while (cursor);
    }
    for (const taskId of ids) {
      const current = await readDocument("task", taskId, props.workspace.rootObjectId);
      if (current.read.object.effectivelyArchived) continue;
      if (!['done', 'cancelled'].includes(current.value.workflowState) || current.value.claim || current.value.publication)
        throw new Error("A task changed. Refresh the board before archiving the remaining tasks.");
      const result = await action.start("Archive task", {
        action: "archive", taskId, expectedRevision: current.pin.revision, archived: true,
      });
      if (!result) {
        error.value = `Archived ${archived} of ${ids.length} tasks. Resolve the archive action before archiving the remaining tasks.`;
        break;
      }
      archived++;
    }
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : "The tasks could not be archived.";
  } finally {
    preparing.value = false;
    refresh();
  }
};
// Backlog rank is the Tasks' order among their workspace siblings.
const reorder = async (id: string, placement: Placement) => {
  if (disabled.value || props.workspace.role !== "user") return;
  preparing.value = true;
  error.value = "";
  const place = async (objectId: string, beforeObjectId: string | null) => {
    if (objectId !== beforeObjectId)
      await client.request("objects.reorder", {
        objectId,
        beforeObjectId,
        mutationId: await newOperationId(client),
      });
  };
  try {
    if ("top" in placement) {
      const first = await client.request("objects.query", {
        contractKey: "task-board/task",
        contractVersions: taskContractVersions,
        where: and(rootWhere(props.workspace.rootObjectId), {
          op: "eq",
          field: "data:/workflowState",
          value: "backlog",
        }),
        orderBy: [
          { field: "object.position", direction: "asc" },
          { field: "object.id", direction: "asc" },
        ],
        limit: 1,
      });
      await place(id, first.items[0]?.objectId ?? null);
    } else if ("after" in placement) {
      // Placing a Task right before its target and then swapping them leaves every other rank as it was.
      await place(id, placement.after);
      await place(placement.after, id);
    } else await place(id, placement.before);
  } catch (cause) {
    error.value =
      cause instanceof Error ? cause.message : "The backlog order could not be saved.";
  } finally {
    preparing.value = false;
    void backlogRef.value?.refresh();
  }
};
const dragged = ref<{ id: string; context: TransitionContext } | null>(null);
const dropAction = (column: string): BoardTransition | undefined => {
  const action = (
    {
      backlog: "backlog",
      todo: "todo",
      in_progress: "in_progress",
      waiting: "waiting",
      done: "done",
    } as Record<string, BoardTransition>
  )[column];
  return dragged.value &&
    action &&
    transitions(dragged.value.context).some((item) => item.action === action)
    ? action
    : undefined;
};
const drop = (column: string) => {
  const action = dropAction(column),
    task = dragged.value;
  dragged.value = null;
  if (task && action) void transition(task.id, action);
};
</script>
<template>
  <PageHeader
    title="Task board"
    :loading="counts.loading.value"
    :updated-at="counts.updatedAt.value"
    ><Button
      v-if="workspace.role === 'user'"
      size="sm"
      :disabled="!available"
      as-child
      ><a
        :href="route('new', { node: workspace.serviceNodeId })"
        aria-label="New task"
        ><Plus aria-hidden="true" /><span class="hidden sm:inline"
          >New task</span
        ></a
      ></Button
    ></PageHeader
  >
  <form
    class="mb-5 flex flex-wrap items-center gap-2"
    role="search"
    aria-label="Filter tasks"
    @submit.prevent="submit"
  >
    <div class="relative min-w-0 flex-1 sm:max-w-72">
      <Search
        class="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground"
        aria-hidden="true"
      />
      <Input
        id="task-search"
        v-model="search"
        :maxlength="4096"
        aria-label="Search tasks"
        placeholder="Search tasks"
        class="pl-8"
      />
    </div>
    <Popover v-model:open="filtersOpen">
      <PopoverTrigger as-child>
        <Button type="button" variant="outline"
          ><ListFilter aria-hidden="true" />Filter<span
            v-if="activeFilters"
            class="rounded-sm bg-muted px-1.5 text-xs"
            >{{ activeFilters }}</span
          ></Button
        >
      </PopoverTrigger>
      <PopoverContent align="start" class="w-72">
        <div class="grid gap-4">
          <Field
            ><Label for="status-filter">Status</Label
            ><OptionSelect id="status-filter" v-model="status" class="w-full"
              ><option value="">All statuses</option>
              <option v-for="value in taskStatuses" :key="value" :value="value">
                {{ stateLabel(value) }}
              </option></OptionSelect
            ></Field
          >
          <Field
            ><Label for="control-filter">Worker</Label
            ><OptionSelect id="control-filter" v-model="control" class="w-full"
              ><option value="">Me and Agent</option>
              <option value="user">Me</option>
              <option value="agent">Agent</option></OptionSelect
            ></Field
          >
          <Field
            ><Label for="host-filter">PC</Label
            ><OptionSelect id="host-filter" v-model="host" class="w-full"
              ><option value="">All PCs</option>
              <option
                v-if="
                  host &&
                  !hosts.value.value?.items.some((item) => item.hostId === host)
                "
                :value="host"
              >
                {{ host }}
              </option>
              <option
                v-for="id in [
                  ...new Set(hosts.value.value?.items.map((item) => item.hostId)),
                ]"
                :key="id"
                :value="id"
              >
                {{ id }}
              </option></OptionSelect
            ></Field
          >
          <Field
            ><Label for="category-filter">Category</Label
            ><Input
              id="category-filter"
              v-model="category"
              placeholder="All categories"
          /></Field>
          <div class="flex justify-end gap-2">
            <Button variant="ghost" size="sm" as-child
              ><a :href="route('tasks', { node: workspace.serviceNodeId })"
                >Clear</a
              ></Button
            ><Button type="button" size="sm" @click="submit">Apply</Button>
          </div>
        </div>
      </PopoverContent>
    </Popover>
    <Popover>
      <PopoverTrigger as-child>
        <Button type="button" variant="outline" aria-label="Display options"
          ><SlidersHorizontal aria-hidden="true" /><span
            class="hidden sm:inline"
            >Display</span
          ></Button
        >
      </PopoverTrigger>
      <PopoverContent align="start" class="grid w-64 gap-4">
        <Field
          ><Label for="swimlane">Board swimlanes</Label
          ><OptionSelect id="swimlane" v-model="swimlane" class="w-full"
            ><option value="none">No swimlanes</option>
            <option value="category">By category</option></OptionSelect
          ></Field
        >
        <Field
          ><Label for="backlog-swimlane">Backlog swimlanes</Label
          ><OptionSelect id="backlog-swimlane" v-model="backlogSwimlane" class="w-full"
            ><option value="none">No swimlanes</option>
            <option value="category">By category</option></OptionSelect
          ></Field
        >
      </PopoverContent>
    </Popover>
    <Button
      v-if="activeFilters || query.get('q')"
      variant="ghost"
      as-child
      ><a :href="route('tasks', { node: workspace.serviceNodeId })"
        ><X aria-hidden="true" />Clear filters</a
      ></Button
    >
  </form>
  <ActionState
    v-if="action.saved.value?.phase !== 'succeeded' || action.error.value"
    :action="action"
    @changed="recovered"
  /><Alert v-if="error" variant="destructive" class="mb-4"
    ><AlertDescription>{{ error }}</AlertDescription></Alert
  >
  <p
    v-if="counts.error.value"
    role="status"
    class="mb-3 text-sm text-muted-foreground"
  >
    Counts unavailable. {{ counts.error.value }}
  </p>
  <p
    v-if="noMatchingTasks"
    role="status"
    class="mb-4 text-sm text-muted-foreground"
  >
    No matching tasks
  </p>
  <div
    v-if="columns.length"
    role="region"
    aria-label="Task board"
    tabindex="0"
    class="task-board"
  >
    <div
      v-for="column in columns"
      :key="column.id + filterKey"
      class="board-lane"
      :class="{ 'drop-target': dropAction(column.id) }"
      @dragover="dropAction(column.id) && $event.preventDefault()"
      @drop.prevent="drop(column.id)"
    >
      <BoardColumn
        ref="columnRefs"
        :workspace="workspace"
        :query="query"
        :column="column"
        :count="
          counts.error.value ? null : (counts.value.value?.[column.id] ?? null)
        "
        :href="(id) => link({ id, panel: 'sheet' })"
        :disabled="disabled"
        :swimlane="swimlane"
        @transition="transition"
        @drag="dragged = $event"
        @archive="archiveTasks"
        @archive-all="archiveTasks()"
      />
    </div>
  </div>
  <div
    v-if="showBacklog"
    class="mt-8 rounded-lg"
    :class="{ 'drop-target': dropAction('backlog') }"
    aria-label="Backlog tasks"
    @dragover="dropAction('backlog') && $event.preventDefault()"
    @drop.prevent="drop('backlog')"
  >
    <BoardColumn
      ref="backlogRef"
      layout="list"
      :workspace="workspace"
      :query="query"
      :column="backlogColumn"
      :count="counts.error.value ? null : (counts.value.value?.backlog ?? null)"
      :href="(id) => link({ id, panel: 'sheet' })"
      :disabled="disabled"
      :swimlane="backlogSwimlane"
      @transition="transition"
      @reorder="reorder"
      @drag="dragged = $event"
    />
  </div>
  <Disclosure
    v-model:open="archiveOpen"
    class="mt-6"
    title="Archive"
    trigger-class="w-auto text-xs"
  >
    <BoardColumn
      ref="archiveRef"
      layout="list"
      archived
      :workspace="workspace"
      :query="query"
      :column="archiveColumn"
      :count="null"
      :href="(id) => link({ id, panel: 'sheet' })"
      :disabled="disabled"
      swimlane="none"
    />
  </Disclosure>
  <Sheet v-model:open="selected"
    ><SheetContent
      class="w-full gap-0 overflow-y-auto p-0 sm:max-w-3xl xl:max-w-5xl [&>button]:hidden"
      @open-auto-focus.prevent
      ><SheetTitle class="sr-only">Task details</SheetTitle
      ><SheetDescription class="sr-only"
        >Review and update the selected task. Close to return to the same
        board.</SheetDescription
      ><TaskDetail
        v-if="query.get('id')"
        :key="query.get('id')!"
        :task-id="query.get('id')!"
        :workspace="workspace"
        :available="available"
        :query="query"
        sheet
        @changed="refresh" @deleted="selected = false; refresh()" /></SheetContent
  ></Sheet>
</template>
<style scoped>
.task-board {
  display: grid;
  gap: 1.5rem;
  min-width: 0;
}
.board-lane {
  min-width: 0;
}
.drop-target {
  outline: 2px dashed var(--ring);
  outline-offset: 2px;
  border-radius: 0.75rem;
}
@media (min-width: 1024px) {
  .task-board {
    display: flex;
    gap: 0.75rem;
    overflow-x: auto;
    align-items: stretch;
    padding-bottom: 0.5rem;
  }
  .task-board .board-lane {
    flex: 1 0 220px;
  }
  .board-lane :deep(.board-column) {
    height: 100%;
    min-height: min(32rem, calc(100dvh - 260px));
  }
}
</style>
