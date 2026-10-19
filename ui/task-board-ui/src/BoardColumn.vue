<script setup lang="ts">
import {
  Archive,
  ArrowUpDown,
  Calendar,
  ChevronDown,
  ChevronsDown,
  ChevronsUp,
  ChevronUp,
  Clock,
  FileCheck2,
  MessageSquare,
  MonitorDot,
  MoreHorizontal,
  Tag,
  User,
} from "@lucide/vue";
import { computed, ref } from "vue";
import {
  Button,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  PageControls,
  RemoteState,
  StatusBadge,
} from "@ivy/ui";
import { usePage } from "../../../packages/ui-client/src/runtime";
import { nativeRead } from "../../../packages/ui-client/src/native";
import { mapConcurrent } from '../../../packages/ui-client/src/concurrency';
import { useCommentVisibility } from './comment-visibility';
import { and, base, client, taskContractVersions } from "./runtime";
import type { TaskBoard } from "./runtime";
import { taskFilter } from "./task-filter";
import {
  conditionLabel,
  conditionTone,
  noteworthyCondition,
  waitingGroup,
  waitingGroups,
  priorityLabel,
  shortDate,
  stateColors,
  stateIcons,
  stateLabel,
  transitions,
} from "./board";
import type { BoardTransition, Placement, TransitionContext } from "./board";
const props = defineProps<{
  workspace: TaskBoard.WorkspaceInfo;
  query: URLSearchParams;
  column: { id: string; title: string; states: string[] };
  count: number | null;
  href: (id: string) => string;
  disabled: boolean;
  pendingTaskId?: string | null;
  archiveDisabled?: boolean;
  swimlane: string;
  layout?: "column" | "list";
  /** Lists archived Tasks of every state instead of one workflow column. */
  archived?: boolean;
}>();
const emit = defineEmits<{
  transition: [id: string, action: BoardTransition];
  drag: [value: { id: string; context: TransitionContext } | null];
  reorder: [id: string, placement: Placement];
  archive: [id: string];
  archiveAll: [];
}>();
// Each column is ordered by hand (the Hive sibling position) or by priority; the backlog starts
// manual, the others by priority. The choice is remembered per browser and column.
const orderKey = "ivy.task-board.order:" + base.href + ":" + props.column.id;
const order = ref<"manual" | "priority">("priority");
try {
  const stored = localStorage.getItem(orderKey);
  order.value = stored === "manual" || stored === "priority" ? stored : props.column.id === "backlog" ? "manual" : "priority";
} catch {
  order.value = props.column.id === "backlog" ? "manual" : "priority";
}
const toggleOrder = () => {
  order.value = order.value === "manual" ? "priority" : "manual";
  try {
    localStorage.setItem(orderKey, order.value);
  } catch {
    /* Optional view state. */
  }
  pages.reset();
};
const manual = computed(() => !props.archived && order.value === "manual");
const ranked = computed(() => manual.value && props.workspace.role === "user");
const showCrossAgent = useCommentVisibility();
const pages = usePage(async (signal, cursor) => {
  const waiting = props.column.id === 'waiting' && !props.archived;
  const load = (cursor: string | null) => client.request('objects.query', {
    contractKey: 'task-board/task', contractVersions: taskContractVersions,
    where: and(taskFilter(props.workspace.rootObjectId, props.query), props.archived
      ? { op: 'eq', field: 'object.effectivelyArchived', value: true }
      : { op: 'in', field: 'data:/workflowState', value: props.column.states }),
    ...(props.archived ? { includeArchived: true } : {}),
    select: ['data:/taskKey'],
    orderBy: props.archived ? [{ field: 'object.updatedAt', direction: 'desc' }, { field: 'object.id', direction: 'asc' }]
      : [...(props.swimlane === 'category' ? [{ field: 'data:/fields/category', direction: 'asc' as const }] : []),
        ...(manual.value ? [{ field: 'object.position', direction: 'asc' as const }]
          : [{ field: 'data:/fields/priority', direction: 'desc' as const }, { field: 'object.createdAt', direction: 'desc' as const }]),
        { field: 'object.id', direction: 'asc' }],
    limit: waiting ? 200 : 20, ...(cursor ? { cursor } : {}),
  }, { signal });
  const result = await load(cursor ?? null);
  // Fetch the entire Waiting lane so a lower-priority User wait never hides behind another group's page.
  const cursors = new Set<string>();
  while (waiting && result.nextCursor) {
    if (cursors.has(result.nextCursor)) throw new Error('Waiting groups could not finish. Refresh the board.');
    cursors.add(result.nextCursor);
    const page = await load(result.nextCursor);
    result.items.push(...page.items); result.nextCursor = page.nextCursor;
  }
  const dependencies = new Map<string, Promise<boolean>>();
  const dependencyDone = (id: string) => {
    if (!dependencies.has(id)) dependencies.set(id, nativeRead(client, props.workspace.serviceNodeId, 'task-board.read', { task: id, view: 'card' }, signal)
      .then(value => (value as TaskBoard.TaskCardView).task.workflowState === 'done').catch(() => { signal.throwIfAborted(); return false; }));
    return dependencies.get(id)!;
  };
  const items = await mapConcurrent(result.items, async item => {
    const view = await nativeRead(client, props.workspace.serviceNodeId, 'task-board.read', { task: item.objectId, view: 'card' }, signal) as TaskBoard.TaskCardView;
    const task = view.task, fields = task.fields;
    const dependency = waiting && (await Promise.all(fields.dependencies.map(dependencyDone))).some(done => !done);
    return {
      id: item.objectId, taskKey: task.taskKey, title: fields.title, category: fields.category ?? '',
      priority: fields.priority, control: fields.control,
      requestedEnvironment: fields.executionRequirement?.kind === 'host' ? fields.executionRequirement.hostId : '',
      resolvedHost: task.claim?.target.hostId ?? '', condition: view.executionCondition.state,
      conditionDetail: view.executionCondition.detail, openQuestionCount: task.openQuestionCount ?? 0,
      checkFailure: task.scheduledCheckFailure, nextReviewAt: fields.nextReviewAt ?? '', dueAt: fields.dueAt ?? '',
      commentCount: view.commentCount, directCommentCount: view.directCommentCount,
      unreadAgentComments: view.unreadAgentComments, unreadDirectAgentComments: view.unreadDirectAgentComments,
      group: waiting ? waitingGroup(task, view.executionCondition, dependency) : '',
      context: { workflowState: task.workflowState, control: fields.control, claimed: !!task.claim,
        waiting: task.waiting?.reason ?? '', hasResult: !!task.latestResult, publishing: !!task.publication },
    };
  }, signal);
  if (waiting) items.sort((a, b) => waitingGroups.findIndex(group => group.id === a.group) - waitingGroups.findIndex(group => group.id === b.group));
  return { nextCursor: result.nextCursor, items };
}, 15000, () => 'ivy.task-board.column:' + JSON.stringify([base.href, order.value, props.workspace.serviceNodeId,
  props.workspace.callerPrincipalId, props.column.id, !!props.archived, props.swimlane,
  ['q', 'status', 'control', 'host', 'category', 'project'].map(key => props.query.get(key))]), ['objects/task-board/task']);

type Item = { id: string; category: string; group: string; context: TransitionContext };
const dragging = ref<Item | null>(null),
  marker = ref<{ id: string; after: boolean } | null>(null);
const startDrag = (event: DragEvent, item: Item) => {
  if (!movable(item)) {
    event.preventDefault();
    return;
  }
  event.dataTransfer?.setData("application/x-ivy-task", item.id);
  if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
  dragging.value = item;
  emit("drag", item);
};
const endDrag = () => {
  dragging.value = null;
  marker.value = null;
  emit("drag", null);
};
const lane = (category: string) => category || "Uncategorized";
const items = computed(() => (pages.value.value?.items ?? []).map(item => ({ ...item,
  commentCount: showCrossAgent.value ? item.commentCount : item.directCommentCount,
  unreadAgentComments: showCrossAgent.value ? item.unreadAgentComments : item.unreadDirectAgentComments,
})));
const groups = computed(() => props.column.id === 'waiting' && !props.archived
  ? waitingGroups.map(group => ({ ...group, items: items.value.filter(item => item.group === group.id) }))
  : [{ id: '', title: '', items: items.value }]);
// Neighbours within the same swimlane, so ranking never jumps across categories.
const sameLane = (a: Item, b: Item | undefined) =>
  !!b && a.group === b.group && (props.swimlane !== "category" || lane(a.category) === lane(b.category));
const previous = (index: number) =>
  sameLane(items.value[index]!, items.value[index - 1]) ? items.value[index - 1] : undefined;
const next = (index: number) =>
  sameLane(items.value[index]!, items.value[index + 1]) ? items.value[index + 1] : undefined;
const moreBelow = (index: number) =>
  !!next(index) || (index === items.value.length - 1 && !!pages.value.value?.nextCursor);
// Dropping on the upper half of a row places the Task before it, on the lower half after it.
const dragOver = (event: DragEvent, task: Item) => {
  const source = dragging.value;
  if (!ranked.value || props.disabled || !source || source.id === task.id || !sameLane(source, task)) return;
  const box = (event.currentTarget as HTMLElement).getBoundingClientRect();
  event.preventDefault();
  event.stopPropagation();
  marker.value = { id: task.id, after: event.clientY > box.top + box.height / 2 };
};
const dropOn = (event: DragEvent) => {
  const source = dragging.value,
    target = marker.value;
  if (!source || !target) return;
  event.preventDefault();
  event.stopPropagation();
  endDrag();
  emit("reorder", source.id, target.after ? { after: target.id } : { before: target.id });
};
const actions = (item: { context: TransitionContext }) =>
  props.archived || props.workspace.role !== "user"
    ? []
    : transitions(item.context);
const taskDisabled = (id: string) => props.disabled || id === props.pendingTaskId;
const movable = (item: { id: string; context: TransitionContext }) =>
  !props.archived &&
  !taskDisabled(item.id) &&
  props.workspace.role === "user" &&
  (ranked.value || transitions(item.context).length > 0);
defineExpose({ refresh: pages.refresh });
</script>
<template>
  <section
    :aria-label="column.title"
    class="board-column min-w-0"
    :class="layout === 'list' ? '' : 'flex flex-col rounded-xl bg-muted/50 p-2'"
  >
    <h2
      v-if="!archived"
      class="flex items-center gap-2 px-1.5 text-sm font-medium"
      :class="layout === 'list' ? 'pb-3' : 'pb-2 pt-1'"
    >
      <component
        :is="stateIcons[column.id] ?? stateIcons.todo"
        class="size-4"
        :class="stateColors[column.id] ?? 'text-muted-foreground'"
        aria-hidden="true"
      />
      <span>{{ column.title }}</span>
      <span
        class="font-normal text-muted-foreground"
        :aria-label="count === null ? 'Counting tasks' : count + ' total tasks'"
      >{{ count ?? "…" }}</span
      >
      <Button
        variant="ghost"
        size="sm"
        class="ml-auto h-7 gap-1 px-2 text-xs font-normal text-muted-foreground"
        :aria-label="'Order ' + column.title + (order === 'manual' ? ' manually' : ' by priority')"
        :aria-pressed="order === 'manual'"
        @click="toggleOrder"
        ><ArrowUpDown aria-hidden="true" />{{ order === "manual" ? "Manual" : "Priority" }}</Button
      >
      <DropdownMenu v-if="column.id === 'done' && workspace.role === 'user'">
        <DropdownMenuTrigger as-child>
          <Button variant="ghost" size="icon-sm" aria-label="Done actions" :disabled="disabled || archiveDisabled">
            <MoreHorizontal aria-hidden="true" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem :disabled="disabled || archiveDisabled || !count" @select="emit('archiveAll')">
            <Archive aria-hidden="true" />Archive all tasks
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </h2>
    <slot name="feedback" />
    <RemoteState
      :loading="pages.loading.value"
      :error="pages.error.value"
      :has-data="!!pages.value.value"
      @retry="pages.refresh"
    />
    <Button
      v-if="pages.error.value && pages.page.value > 1"
      variant="ghost"
      size="sm"
      @click="pages.reset"
      >Restart from first page</Button
    >
    <div
      :class="
        layout === 'list'
          ? 'overflow-hidden rounded-lg border bg-card'
          : 'flex flex-col gap-2'
      "
    >
      <section v-for="group in groups" :key="group.id" :aria-label="group.title || undefined"
        :data-waiting-group="group.id || undefined" class="flex flex-col gap-2"
        :class="group.id === 'user' ? 'rounded-lg border-2 border-amber-500/70 p-2' : ''">
      <h3 v-if="group.title" class="px-1.5 pt-2 text-xs font-semibold text-muted-foreground">{{ group.title }} <span class="font-normal">{{ group.items.length }}</span></h3>
      <template
        v-for="task in group.items"
        :key="task.id"
        ><h3
          v-if="
            swimlane === 'category' &&
            !archived &&
            (group.items.indexOf(task) === 0 ||
              lane(task.category) !==
                lane(pages.value.value!.items[items.indexOf(task) - 1]!.category))
          "
          class="px-1.5 pt-2 text-xs font-medium text-muted-foreground"
          :class="{ 'border-b bg-muted/40 px-3 py-1.5': layout === 'list' }"
        >
          {{ lane(task.category) }}
        </h3>
        <article
          class="task-item group relative min-w-0"
          :class="[
            layout === 'list'
              ? 'flex items-center gap-3 border-b px-3 py-2 last:border-b-0 hover:bg-muted/50'
              : 'rounded-lg border bg-card p-3 shadow-xs transition-colors hover:border-foreground/20',
            marker?.id === task.id ? (marker.after ? 'drop-after' : 'drop-before') : '',
          ]"
          :draggable="movable(task)"
          @dragstart="startDrag($event, task)"
          @dragend="endDrag"
          @dragover="dragOver($event, task)"
          @dragleave="marker?.id === task.id && (marker = null)"
          @drop="dropOn"
        >
          <span
            v-if="layout === 'list'"
            class="hidden w-20 shrink-0 text-xs text-muted-foreground sm:block"
            >{{ task.taskKey }}</span
          >
          <a
            :href="href(task.id)"
            class="min-w-0 flex-1 rounded-sm outline-none after:absolute after:inset-0 focus-visible:after:rounded-lg focus-visible:after:ring-3 focus-visible:after:ring-ring/50"
            ><h3
              class="text-sm font-medium leading-snug"
              :class="layout === 'list' ? 'truncate' : 'break-words pr-6'"
            >
              {{ task.title }}
            </h3></a
          >
          <div
            class="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted-foreground"
            :class="layout === 'list' ? 'hidden shrink-0 md:flex' : 'mt-2'"
          >
            <span v-if="layout !== 'list'">{{ task.taskKey }}</span>
            <span
              v-if="typeof task.priority === 'number' && task.priority !== 2"
              class="inline-flex items-center gap-1"
              :class="{ 'text-foreground': task.priority >= 3 }"
              ><component
                :is="
                  task.priority >= 4
                    ? ChevronsUp
                    : task.priority === 3
                      ? ChevronUp
                      : task.priority === 1
                        ? ChevronDown
                        : ChevronsDown
                "
                class="size-3.5"
                aria-hidden="true"
              />{{ priorityLabel(task.priority) }}</span
            >
            <span
              v-if="task.category && swimlane !== 'category'"
              class="inline-flex items-center gap-1"
              ><Tag class="size-3.5" aria-hidden="true" />{{
                task.category
              }}</span
            >
            <span
              v-if="task.control === 'user'"
              class="inline-flex items-center gap-1"
              ><User class="size-3.5" aria-hidden="true" />Me</span
            >
            <span
              v-if="task.resolvedHost || task.requestedEnvironment"
              class="inline-flex items-center gap-1"
              ><MonitorDot class="size-3.5" aria-hidden="true" />{{
                task.resolvedHost || task.requestedEnvironment
              }}</span
            >
            <span
              v-if="task.dueAt"
              class="inline-flex items-center gap-1"
              :title="'Due ' + new Date(task.dueAt).toLocaleString()"
              ><Calendar class="size-3.5" aria-hidden="true" />{{
                shortDate(task.dueAt)
              }}</span
            >
            <span
              v-if="task.nextReviewAt"
              class="inline-flex items-center gap-1"
              :title="'Next check ' + new Date(task.nextReviewAt).toLocaleString()"
              ><Clock class="size-3.5" aria-hidden="true" />{{
                shortDate(task.nextReviewAt)
              }}</span
            >
            <span
              v-if="task.commentCount"
              class="inline-flex items-center gap-1"
              :class="task.unreadAgentComments ? 'font-medium text-foreground' : ''"
              :title="task.commentCount + ' comments'"
              :aria-label="
                task.unreadAgentComments +
                ' unread agent comment' +
                (task.unreadAgentComments === 1 ? '' : 's')
              "
              ><MessageSquare class="size-3.5" aria-hidden="true" />{{
                task.commentCount
              }}</span
            >
            <span
              v-if="task.context.hasResult || task.context.publishing"
              class="inline-flex items-center gap-1"
              ><FileCheck2 class="size-3.5" aria-hidden="true" />{{
                task.context.hasResult ? "Result" : "Publishing"
              }}</span
            >
            <span v-if="archived">{{
              stateLabel(task.context.workflowState)
            }}</span>
            <span v-else-if="task.context.workflowState === 'cancelled'"
              >Cancelled</span
            >
          </div>
          <StatusBadge
            v-if="noteworthyCondition(task.condition) && !['blocked_dependency', 'blocked_environment', 'needs_user', 'deferred'].includes(task.condition)"
            class="relative"
            :class="layout === 'list' ? 'shrink-0' : 'mt-2'"
            :label="conditionLabel(task.condition)"
            :tone="conditionTone(task.condition)"
            :title="task.conditionDetail"
          />
          <StatusBadge v-if="task.checkFailure" label="Check failed" tone="bad" class="mt-2" :title="task.checkFailure" />
          <DropdownMenu
            ><DropdownMenuTrigger as-child
              ><Button
                variant="ghost"
                size="icon-sm"
                class="task-actions relative z-10 shrink-0"
                :class="{ 'absolute! top-2 right-2': layout !== 'list' }"
                :aria-label="'Actions for ' + task.title"
                ><MoreHorizontal aria-hidden="true" /></Button></DropdownMenuTrigger
            ><DropdownMenuContent align="end"
              ><DropdownMenuItem as-child
                ><a :href="href(task.id)">Open task</a></DropdownMenuItem
              ><DropdownMenuItem
                v-for="item in actions(task)"
                :key="item.action"
                :disabled="taskDisabled(task.id)"
                @select="emit('transition', task.id, item.action)"
                >{{ item.label }}</DropdownMenuItem
              ><template v-if="ranked"
                ><DropdownMenuSeparator /><DropdownMenuItem
                  v-if="previous(items.indexOf(task)) || pages.page.value > 1"
                  :disabled="taskDisabled(task.id)"
                  @select="emit('reorder', task.id, { top: column.states })"
                  >Move to top</DropdownMenuItem
                ><DropdownMenuItem
                  v-if="previous(items.indexOf(task))"
                  :disabled="taskDisabled(task.id)"
                  @select="emit('reorder', task.id, { before: previous(items.indexOf(task))!.id })"
                  >Move up</DropdownMenuItem
                ><DropdownMenuItem
                  v-if="next(items.indexOf(task))"
                  :disabled="taskDisabled(task.id)"
                  @select="emit('reorder', task.id, { after: next(items.indexOf(task))!.id })"
                  >Move down</DropdownMenuItem
                ><DropdownMenuItem
                  v-if="moreBelow(items.indexOf(task))"
                  :disabled="taskDisabled(task.id)"
                  @select="emit('reorder', task.id, { before: null })"
                  >Move to bottom</DropdownMenuItem
                ></template
              ><template v-if="!archived && workspace.role === 'user' && ['done', 'cancelled'].includes(task.context.workflowState)">
                <DropdownMenuSeparator />
                <DropdownMenuItem :disabled="taskDisabled(task.id) || archiveDisabled || task.context.claimed || task.context.publishing" @select="emit('archive', task.id)">
                  <Archive aria-hidden="true" />Archive task
                </DropdownMenuItem>
              </template></DropdownMenuContent
            ></DropdownMenu
          >
        </article></template
      >
      </section>
      <p
        v-if="pages.value.value?.items.length === 0"
        class="px-3 py-6 text-center text-xs text-muted-foreground"
      >
        No tasks
      </p>
    </div>
    <PageControls
      v-if="pages.value.value?.nextCursor || pages.page.value > 1"
      :count="pages.value.value?.items.length ?? 0"
      :page="pages.page.value"
      :has-next="!!pages.value.value?.nextCursor"
      :loading="pages.loading.value"
      @next="pages.next"
      @previous="pages.previous"
    />
  </section>
</template>
<style scoped>
.task-actions {
  opacity: 0;
}
.task-item:hover .task-actions,
.task-item:focus-within .task-actions,
.task-actions[aria-expanded="true"] {
  opacity: 1;
}
@media (hover: none) {
  .task-actions {
    opacity: 1;
  }
}
.drop-before {
  box-shadow: inset 0 2px 0 var(--ring);
}
.drop-after {
  box-shadow: inset 0 -2px 0 var(--ring);
}
</style>
