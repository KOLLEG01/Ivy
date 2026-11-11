<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, shallowRef, watch } from "vue";
import {
  Alert,
  AlertDescription,
  Button,
  Disclosure,
  Field,
  Input,
  HorizontalScrollIndicator,
  Label,
  OptionSelect,
  PageHeader,
  Popover,
  PopoverContent,
  PopoverTrigger,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogTitle,
  useRemote,
} from "@ivy/ui";
import { ChevronRight, ListFilter, Plus, Search, SlidersHorizontal, X } from "@lucide/vue";
import { route } from "../../../packages/ui-client/src/runtime";
import { objectParentScope } from "../../../packages/ui-client/src/live";
import { newOperationId } from "../../../packages/sdk/src/client.js";
import {
  and,
  base,
  client,
  readDocument,
  rootWhere,
  taskStatuses,
  tr,
} from "./runtime";
import type { Operation, TaskBoard } from "./runtime";
import { useAction } from "./action";
import { useBoardActions } from "./board-actions";
import type { TaskActions } from './board-actions';
import { archiveBoardCard, archiveBoardSelection, readArchiveCards } from './board-archive';
import ActionState from "./ActionState.vue";
import { useObjectArchive } from "../../../packages/ui-client/src/object-archive";
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
import { useCommentPreferences } from './comment-preferences';
const props = defineProps<{
  workspace: TaskBoard.WorkspaceInfo;
  query: URLSearchParams;
  available: boolean;
}>();
const emit = defineEmits<{ host: [host: string | null | undefined]; agentNode: [node: string | undefined] }>();
const { preferences } = useCommentPreferences(() => props.workspace);
const boardElement = ref<HTMLElement | null>(null);
const categories = ref<string[]>([]);
const boardLanes = computed(() => swimlane.value === 'category' ? categories.value.length ? categories.value : ['Uncategorized'] : ['']);
const collapsedLanes = ref<string[]>([]), backlogOpen = ref(true);
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
try {
  const retained = JSON.parse(localStorage.getItem(swimlaneKey + ':collapsed') ?? '[]');
  if (Array.isArray(retained)) collapsedLanes.value = retained.filter(value => typeof value === 'string');
  backlogOpen.value = localStorage.getItem(swimlaneKey + ':backlog-open') !== 'false';
} catch { /* Optional view state. */ }
watch(collapsedLanes, value => { try { localStorage.setItem(swimlaneKey + ':collapsed', JSON.stringify(value)); } catch { /* Optional view state. */ } });
watch(backlogOpen, value => { try { localStorage.setItem(swimlaneKey + ':backlog-open', String(value)); } catch { /* Optional view state. */ } });
const toggleLane = (lane: string) => { collapsedLanes.value = collapsedLanes.value.includes(lane) ? collapsedLanes.value.filter(value => value !== lane) : [...collapsedLanes.value, lane]; };
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
    ["q", "status", "control", "host", "category", "project"].map((key) =>
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
  const states = new Map<string, number>(),
    lanes = new Set<string>(),
    cursors = new Set<string>();
  let cursor: string | null = null;
  do {
    const page: Operation.ObjectsCountResult = await client.request(
      "objects.count",
      {
        contractKey: "task-board/task",
        where: taskFilter(props.workspace.rootObjectId, props.query),
        groupBy: ["data:/workflowState", "data:/fields/category"],
        limit: 200,
        ...(cursor ? { cursor } : {}),
      },
      { signal },
    );
    for (const item of page.items) {
      const state = String(item.values["data:/workflowState"]);
      states.set(state, (states.get(state) ?? 0) + item.count);
      if (item.values['data:/workflowState'] !== 'backlog') lanes.add(String(item.values['data:/fields/category'] || 'Uncategorized'));
    }
    cursor = page.nextCursor;
    if (cursor && cursors.has(cursor))
      throw new Error("Task counts could not finish. Refresh the board.");
    if (cursor) cursors.add(cursor);
  } while (cursor);
  categories.value = [...lanes].sort((a, b) => a.localeCompare(b));
  return Object.fromEntries(
    [...boardColumns, backlogColumn].map((column) => [
      column.id,
      [...states].reduce((total, [state, count]) => total + (column.states.includes(state) ? count : 0), 0),
    ]),
  );
}, 30000, () => [objectParentScope('task-board/task', props.workspace.rootObjectId)], () => props.available);
watch(filterKey, () => {
  observedColumns.clear();
  boardActions.clearCards();
  counts.value.value = null;
  void counts.refresh();
  search.value = props.query.get("q") ?? "";
  status.value = props.query.get("status") ?? "";
  control.value = props.query.get("control") ?? "";
  host.value = props.query.get("host") ?? "";
  category.value = props.query.get("category") ?? "";
});
const retainedBoardAction = useAction(() => props.workspace, "board"),
  workflowAction = useAction(() => props.workspace, "board:workflow");
// An older retained archive must keep its original slot while other tickets move.
const action = workflowAction.saved.value || retainedBoardAction.saved.value?.call.qualifiedName === "task-board.archive"
  ? workflowAction : retainedBoardAction;
const boardActions = useBoardActions(() => props.workspace, synchronizeActionSlots, findDisplayedCard, () => props.query.get('id'));
const observedColumns = new Set<string>();
const releaseClosedActions = () => {
  if (columns.value.every(column => observedColumns.has(column.id)) &&
      (!showBacklog.value || !backlogOpen.value || observedColumns.has('backlog')) && (!archiveOpen.value || observedColumns.has('archive')))
    void boardActions.release().catch(() => {});
};
const observeCards = (columnId: string, cards: TaskBoard.TaskCardView[]) => {
  boardActions.observe(cards);
  // Every open column must receive its ordinary first page before cleanup can
  // join a batch. Earlier empty columns cannot delay a later visible column.
  observedColumns.add(columnId);
  releaseClosedActions();
};
const detailBoardAction = computed(() => boardActions.entries.value.find(entry => entry.taskId === props.query.get('id'))?.action);
const detailBoardArchive = computed(() => detailBoardAction.value?.identityMatches.value &&
  detailBoardAction.value.saved.value?.phase === 'succeeded' ? detailBoardAction.value.saved.value.outcome?.archive ?? null : null);
const detailBoardProjection = computed(() => boardActions.projections.value.find(value =>
  value.card.object.objectId === props.query.get('id')) ?? null);
const columnRefs = ref<InstanceType<typeof BoardColumn>[]>([]),
  backlogRef = ref<InstanceType<typeof BoardColumn> | null>(null),
  archiveRef = ref<InstanceType<typeof BoardColumn> | null>(null),
  archive = useObjectArchive(client, "ivy:task-board:archive:" + base.href + ":" + props.workspace.serviceNodeId + ":board"),
  archiveError = ref(""),
  error = ref(""),
  preparing = ref(false),
  archiving = ref(false);
let archiveSelection: AbortController | null = null;
function findDisplayedCard(id: string) {
  for (const column of [...columnRefs.value, backlogRef.value, archiveRef.value]) {
    const card = column?.findCard(id);
    if (card) return card;
  }
  return null;
}
async function synchronizeActionSlots(ids: string[]) {
  if (document.hidden) return false;
  const filter = filterKey.value;
  const cards = await readArchiveCards(props.workspace, ids);
  if (cards.length !== ids.length || filter !== filterKey.value) return false;
  const observation = cards[0]?.observation;
  if (!observation || cards.some(card => card.observation?.runtimeEpoch !== observation.runtimeEpoch || !Number.isSafeInteger(card.observation?.sequence))) return false;
  const fence = { runtimeEpoch: observation.runtimeEpoch, sequence: Math.max(...cards.map(card => card.observation!.sequence)) };
  boardActions.observe(cards);
  const columns = [...columnRefs.value, backlogRef.value, archiveRef.value].filter(column => !!column);
  return !!columns.length && (await Promise.all(columns.map(column => column.settle(fence)))).every(Boolean) && filter === filterKey.value;
}
const retainedArchive = computed(() => retainedBoardAction.saved.value?.call.qualifiedName === "task-board.archive");
// Resolve an already retained archive without showing workflow internals on the board.
useRemote(async () => {
  if (!retainedArchive.value || !retainedBoardAction.identityMatches.value) return;
  if (!retainedBoardAction.canDismiss.value) await retainedBoardAction.reconcile();
  if (retainedBoardAction.canDismiss.value) retainedBoardAction.dismiss();
  refresh();
}, 15000);
const archiveFeedback = computed(() => archiveError.value || archive.error.value ||
  (retainedArchive.value && !retainedBoardAction.busy.value ? retainedBoardAction.error.value || retainedBoardAction.saved.value?.detail : "") ||
  (archive.pending.value ? tr("Archivierung noch nicht bestätigt.", "Archive action awaiting confirmation.") : ""));
const archivePending = computed(() => !!archive.pending.value || retainedArchive.value && !retainedBoardAction.canDismiss.value);
const pendingArchiveTaskId = computed(() => archive.pending.value?.objectId ??
  (retainedArchive.value ? (retainedBoardAction.saved.value?.call.arguments as TaskBoard.ArchiveRequest)?.taskId : null) ?? null);
watch(() => archive.pending.value, (current, previous) => { if (previous && !current) refresh(); });
const retryArchive = async () => {
  if (!props.available || archiving.value || preparing.value || props.workspace.role !== "user") return;
  if (retainedArchive.value) {
    await retainedBoardAction.reconcile();
    if (!retainedBoardAction.canDismiss.value) await retainedBoardAction.replay();
    if (retainedBoardAction.canDismiss.value) retainedBoardAction.dismiss();
  } else if (archive.pending.value) {
    const pending = archive.pending.value;
    archive.clearError();
    await archive.run(pending.objectId, pending.archived);
  }
  refresh();
};
const refresh = () => {
  counts.invalidate();
  for (const column of columnRefs.value) void column.refresh();
  void backlogRef.value?.refresh();
  void archiveRef.value?.refresh();
};
const recovered = () => {
  if (action.saved.value?.phase === "succeeded") error.value = "";
  refresh();
};
watch(() => ({ selected: props.query.get('id'), active: boardActions.active.value,
  entries: [...boardActions.entries.value, ...boardActions.detailEntries.value].map(({ taskId, action }) =>
    ({ taskId, operationId: action.saved.value?.call.operationId, phase: action.saved.value?.phase, busy: action.busy.value, error: action.error.value })) }), (current, previous) => {
  if (current.entries.some(({ taskId, operationId, phase }) => phase === 'succeeded' &&
    !previous?.entries.some(entry => entry.taskId === taskId && entry.operationId === operationId && entry.phase === 'succeeded') &&
    !boardActions.active.value.includes(taskId))) refresh();
  // Release closed, confirmed owners in the background. Failed reads keep their
  // original records for the next view change or ordinary capacity check.
  releaseClosedActions();
});
const detailOwner = shallowRef<{ taskId: string; actions: TaskActions | null; issue: string } | null>(null);
watch(() => props.query.get('id'), async (taskId, _previous, onCleanup) => {
  detailOwner.value = null;
  if (!taskId) return;
  let current = true;
  onCleanup(() => { current = false; });
  try {
    const actions = await boardActions.detail(taskId);
    if (current) detailOwner.value = { taskId, actions, issue: '' };
  } catch (cause) {
    if (current) detailOwner.value = { taskId, actions: null,
      issue: cause instanceof Error ? cause.message : 'Resolve a previous task change before starting more changes in this tab.' };
  }
}, { immediate: true });
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
let returnFocus: HTMLElement | null = null;
const originalOverflow = document.documentElement.style.overflow;
watch(selected, async (open, previous) => {
  if (!open) emit('host', undefined);
  if (!open) emit('agentNode', undefined);
  if (open && !previous) returnFocus = document.activeElement as HTMLElement | null;
  document.documentElement.style.overflow = open ? 'hidden' : originalOverflow;
  if (!open) {
    await nextTick();
    (returnFocus?.isConnected ? returnFocus : document.getElementById('main-content'))?.focus({ preventScroll: true });
  }
}, { immediate: true });
onBeforeUnmount(() => { archiveSelection?.abort(); document.documentElement.style.overflow = originalOverflow; });
const disabled = computed(
  () => !props.available || preparing.value,
);
const blockedTaskIds = computed(() => [...new Set([
  ...boardActions.blocked.value,
  ...[retainedBoardAction, workflowAction].flatMap(legacy => {
    const taskId = (legacy.saved.value?.call.arguments as { taskId?: string } | undefined)?.taskId;
    return taskId && legacy.locked.value ? [taskId] : [];
  }),
])]);
const transition = async (id: string, operation: BoardTransition, card?: TaskBoard.TaskCardView) => {
  if (disabled.value || id === pendingArchiveTaskId.value || blockedTaskIds.value.includes(id) || props.workspace.role !== "user") return;
  for (const column of [...columnRefs.value, backlogRef.value]) column?.reveal(operation);
  if (operation === 'backlog') backlogOpen.value = true;
  await boardActions.run(id, async action => {
    await action.start("Move task to " + operation, async () => {
      const current = await readDocument(
        "task", id, props.workspace.rootObjectId, undefined, { limit: 0 },
      );
      if (current.read.object.effectivelyArchived || !transitions(taskContext(current.value)).some(item => item.action === operation))
        throw new Error("This task changed. Open it to review the available actions.");
      return {
        action: "transition", taskId: id, expectedRevision: current.pin.revision, workflowState: operation, detail: null,
      };
    });
    refresh();
  }, card ? { card, state: operation } : undefined);
};
const archiveTasks = async (id?: string, card?: TaskBoard.TaskCardView) => {
  if (disabled.value || props.workspace.role !== 'user') return;
  if (id) {
    if (!card || card.object.objectId !== id || id === pendingArchiveTaskId.value || blockedTaskIds.value.includes(id)) return;
    await archiveBoardCard(() => props.workspace, boardActions, card, refresh);
    return;
  }
  if (archiving.value) return;
  archiving.value = true;
  archiveError.value = '';
  const controller = new AbortController();
  archiveSelection = controller;
  try {
    const result = await archiveBoardSelection(() => props.workspace, props.query, boardActions, refresh,
      taskId => taskId === pendingArchiveTaskId.value || blockedTaskIds.value.includes(taskId), controller.signal);
    if (result.skipped) archiveError.value = tr('Einige Aufgaben haben sich geändert oder werden noch bestätigt. Sie wurden unverändert gelassen.',
      'Some tasks changed or still have changes being confirmed. They were left unchanged.');
  } catch (cause) {
    if (!controller.signal.aborted) archiveError.value = cause instanceof Error ? cause.message : 'The tasks could not be archived.';
  } finally {
    if (archiveSelection === controller) archiveSelection = null;
    archiving.value = false;
    refresh();
  }
};
// Manual rank is the Tasks' order among their workspace siblings.
const reorder = async (id: string, placement: Placement) => {
  if (disabled.value || id === pendingArchiveTaskId.value || blockedTaskIds.value.includes(id) || props.workspace.role !== "user") return;
  if ('after' in placement && (blockedTaskIds.value.includes(placement.after) || placement.after === pendingArchiveTaskId.value)) return;
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
        where: and(rootWhere(props.workspace.rootObjectId), {
          op: "in",
          field: "data:/workflowState",
          value: placement.top,
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
      cause instanceof Error ? cause.message : "The task order could not be saved.";
  } finally {
    preparing.value = false;
    refresh();
  }
};
const dragged = ref<{ id: string; context: TransitionContext; card: TaskBoard.TaskCardView } | null>(null);
const dropAction = (column: string): BoardTransition | undefined => {
  const action = (
    {
      backlog: "backlog",
      in_progress: "in_progress",
      // Dropping into Waiting queues work; it does not invent a blocker.
      waiting: "todo",
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
  if (task && action) void transition(task.id, action, task.card);
};
</script>
<template>
  <div data-slot="task-board-background" :inert="selected" :aria-hidden="selected || undefined">
  <PageHeader
    v-if="!selected"
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
    v-if="!action.busy.value && (action.saved.value?.phase !== 'succeeded' || action.error.value)"
    :action="action"
    @changed="recovered"
  /><Alert v-if="error" variant="destructive" class="mb-4"
    ><AlertDescription>{{ error }}</AlertDescription></Alert
  >
  <Alert v-if="boardActions.recoveryError.value" class="mb-4"><AlertDescription>{{ boardActions.recoveryError.value }}</AlertDescription></Alert>
  <Alert v-if="boardActions.error.value" variant="destructive" class="mb-4"><AlertDescription>
    {{ boardActions.error.value.message }}
    <Button variant="link" size="sm" as-child><a :href="link({ id: boardActions.error.value.taskId, panel: 'sheet' })">{{ tr('Aufgabe öffnen', 'Open task') }}</a></Button>
  </AlertDescription></Alert>
  <template v-for="entry in boardActions.entries.value" :key="entry.taskId">
    <Alert v-if="!entry.action.busy.value && (entry.action.error.value || entry.action.saved.value && entry.action.saved.value.phase !== 'succeeded')" class="mb-4">
      <AlertDescription>
        <Button variant="link" size="sm" as-child><a :href="link({ id: entry.taskId, panel: 'sheet' })">{{ entry.action.saved.value?.label ?? tr('Aufgabe öffnen', 'Open task') }}</a></Button>
        {{ entry.action.error.value || (entry.action.saved.value?.phase === 'failed' ? entry.action.saved.value.detail : tr('Änderung noch nicht bestätigt. Sie wird automatisch geprüft.', 'Change not yet confirmed. It is checked automatically.')) }}
        <Disclosure :title="tr('Details', 'Details')"><ActionState :action="entry.action" @changed="refresh" /></Disclosure>
      </AlertDescription>
    </Alert>
  </template>
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
  <HorizontalScrollIndicator :target="boardElement" attention-selector="[data-needs-user=true]" class="sticky top-[var(--ivy-content-top,3rem)] z-10 bg-background"
    :label="tr('Spalten scrollen', 'Scroll columns')" :left-attention-label="tr('Ein Ticket links benötigt dich', 'A ticket to the left needs you')" :right-attention-label="tr('Ein Ticket rechts benötigt dich', 'A ticket to the right needs you')" />
  <div
    v-if="columns.length"
    role="region"
    aria-label="Task board"
    tabindex="0"
    class="task-board"
    ref="boardElement"
  >
    <div class="task-board-grid" :style="{ gridTemplateColumns: 'repeat(' + columns.length + ', minmax(16rem, 1fr))' }">
    <template v-if="swimlane === 'category'">
      <div v-for="(lane, index) in boardLanes" :key="lane" class="board-swimlane-heading" :style="{ gridRow: 2 + index * 2 }">
        <Button variant="ghost" size="sm" class="sticky left-0 h-8 max-w-[80vw] justify-start" :aria-expanded="!collapsedLanes.includes(lane)" @click="toggleLane(lane)">
          <ChevronRight class="size-4 transition-transform" :class="{ 'rotate-90': !collapsedLanes.includes(lane) }" aria-hidden="true" /><span class="truncate">{{ lane === 'Uncategorized' ? tr('Ohne Kategorie', 'Uncategorized') : lane }}</span>
        </Button>
      </div>
    </template>
    <div
      v-for="(column, columnIndex) in columns"
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
        :active="available"
        :pending-task-id="pendingArchiveTaskId"
        :blocked-task-ids="blockedTaskIds"
        :projections="boardActions.projections.value"
        :archive-all-disabled="archiving"
        :swimlane="swimlane"
        :lanes="boardLanes"
        :collapsed-lanes="collapsedLanes"
        :grid-column="columnIndex + 1"
        @transition="transition"
        @cards="cards => observeCards(column.id, cards)"
        @reorder="reorder"
        @drag="dragged = $event"
        @archive="archiveTasks"
        @archive-all="archiveTasks()"
      >
        <template v-if="column.id === 'done'" #feedback>
          <Alert v-if="archiveFeedback && !archive.busy.value && !retainedBoardAction.busy.value" variant="destructive">
            <AlertDescription>
              {{ archiveFeedback }}
              <Button v-if="pendingArchiveTaskId" variant="link" size="sm" as-child>
                <a :href="link({ id: pendingArchiveTaskId, panel: 'sheet' })">{{ tr("Aufgabe öffnen", "Open task") }}</a>
              </Button>
              <Button v-if="archivePending" variant="outline" size="sm"
                :disabled="!available || preparing || archive.busy.value || retainedBoardAction.busy.value || !retainedBoardAction.identityMatches.value"
                @click="retryArchive">{{ tr("Archivierung erneut versuchen", "Retry archive") }}</Button>
            </AlertDescription>
          </Alert>
        </template>
      </BoardColumn>
    </div>
    </div>
  </div>
  <Disclosure
    v-if="showBacklog"
    v-model:open="backlogOpen"
    class="mt-8 rounded-lg"
    :class="{ 'drop-target': dropAction('backlog') }"
    aria-label="Backlog tasks"
    @dragover="dropAction('backlog') && $event.preventDefault()"
    @drop.prevent="drop('backlog')"
    :title="'Backlog · ' + (counts.value.value?.backlog ?? '…')"
    trigger-class="font-medium text-foreground"
  >
    <BoardColumn
      ref="backlogRef"
      layout="list"
      :workspace="workspace"
      :query="query"
      :column="backlogColumn"
      :active="available && backlogOpen"
      :count="counts.error.value ? null : (counts.value.value?.backlog ?? null)"
      :href="(id) => link({ id, panel: 'sheet' })"
      :disabled="disabled"
      :pending-task-id="pendingArchiveTaskId"
      :blocked-task-ids="blockedTaskIds"
      :projections="boardActions.projections.value"
      :swimlane="backlogSwimlane"
      @transition="transition"
      @cards="cards => observeCards('backlog', cards)"
      @reorder="reorder"
      @drag="dragged = $event"
    />
  </Disclosure>
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
      :active="available && archiveOpen"
      :count="null"
      :href="(id) => link({ id, panel: 'sheet' })"
      :disabled="disabled"
      :projections="boardActions.projections.value"
      @cards="cards => observeCards('archive', cards)"
      swimlane="none"
    />
  </Disclosure>
  </div>
  <Dialog v-model:open="selected" :modal="false"
    ><DialogContent :variant="preferences.ticketLayout === 'fullscreen' ? 'workspace' : 'panel'" :show-close-button="false"
      @interact-outside.prevent @close-auto-focus.prevent
      ><DialogTitle class="sr-only">Task details</DialogTitle
      ><DialogDescription class="sr-only"
        >Review and update the selected task. Close to return to the same
        board.</DialogDescription
      ><TaskDetail
        v-if="query.get('id') && detailOwner?.taskId === query.get('id')"
        :key="query.get('id')!"
        :task-id="query.get('id')!"
        :workspace="workspace"
        :available="available && !detailOwner.issue"
        :action-issue="detailOwner.issue"
        :task-actions="detailOwner.actions"
        :board-blocked="boardActions.boardBlocked.value.includes(query.get('id')!)"
        :board-archive="detailBoardArchive"
        :board-projection="detailBoardProjection"
        :query="query"
        @host="emit('host', $event)"
        @agent-node="emit('agentNode', $event)"
        @close="selected = false"
        @changed="refresh" @deleted="selected = false; refresh()" /></DialogContent
  ></Dialog>
</template>
<style scoped>
.task-board {
  min-width: 0;
  overflow-x: auto;
  overscroll-behavior-x: contain;
  scrollbar-width: thin;
  padding-bottom: .5rem;
}
.task-board-grid {
  display: grid;
  gap: .5rem .75rem;
  min-width: 0;
  align-items: stretch;
}
.board-lane { display: contents; }
.board-swimlane-heading { grid-column: 1 / -1; min-width: 0; border-block: 1px solid var(--border); background: var(--muted); }
.drop-target {
  outline: 2px dashed var(--ring);
  outline-offset: 2px;
  border-radius: 0.75rem;
}
</style>
