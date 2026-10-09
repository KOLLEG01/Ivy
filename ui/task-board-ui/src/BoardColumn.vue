<script setup lang="ts">
import {
  Archive,
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
import { text } from "../../../packages/ui-client/src/native";
import { and, base, client, rootWhere, taskContractVersions } from "./runtime";
import type { TaskBoard } from "./runtime";
import { taskFilter } from "./task-filter";
import {
  conditionLabel,
  conditionTone,
  noteworthyCondition,
  priorityLabel,
  shortDate,
  stateColors,
  stateIcons,
  stateLabel,
  transitions,
} from "./board";
import type { BoardTransition, Placement, TransitionContext } from "./board";
import { executionCondition } from "../../../services/task-board/src/runtime/condition";
const props = defineProps<{
  workspace: TaskBoard.WorkspaceInfo;
  query: URLSearchParams;
  column: { id: string; title: string; states: string[] };
  count: number | null;
  href: (id: string) => string;
  disabled: boolean;
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
// The backlog is ranked by hand; every other column follows priority.
const ranked = computed(
  () => props.column.id === "backlog" && !props.archived && props.workspace.role === "user",
);
const pages = usePage(async (signal, cursor) => {
    const result = await client.request(
      "objects.query",
      {
        contractKey: "task-board/task",
        contractVersions: taskContractVersions,
        where: and(
          taskFilter(props.workspace.rootObjectId, props.query),
          props.archived
            ? { op: "eq", field: "object.effectivelyArchived", value: true }
            : { op: "in", field: "data:/workflowState", value: props.column.states },
        ),
        ...(props.archived ? { includeArchived: true } : {}),
        select: [
          "data:/taskKey",
          "data:/fields/title",
          "data:/fields/category",
          "data:/fields/control",
          "data:/fields/priority",
          "data:/fields/executionRequirement/kind",
          "data:/fields/executionRequirement/hostId",
          "data:/fields/nextReviewAt",
          "data:/fields/dueAt",
          "data:/workflowState",
          "data:/waiting/reason",
          "data:/waiting/detail",
          "data:/claim/phase",
          "data:/claim/target/hostId",
          "data:/publication",
          "data:/latestResult",
          "data:/agentCommentCount",
        ],
        orderBy: props.archived
          ? [
              { field: "object.updatedAt", direction: "desc" },
              { field: "object.id", direction: "asc" },
            ]
          : props.column.id === "backlog"
            ? [
                ...(props.swimlane === "category"
                  ? [{ field: "data:/fields/category", direction: "asc" as const }]
                  : []),
                { field: "object.position", direction: "asc" },
                { field: "object.id", direction: "asc" },
              ]
          : props.swimlane === "category"
            ? [
                { field: "data:/fields/category", direction: "asc" },
                { field: "data:/fields/priority", direction: "desc" },
                { field: "object.id", direction: "asc" },
              ]
            : [
                { field: "data:/fields/priority", direction: "desc" },
                { field: "object.createdAt", direction: "desc" },
                { field: "object.id", direction: "asc" },
              ],
        limit: 20,
        ...(cursor ? { cursor } : {}),
      },
      { signal },
    );
    const ids = result.items.map((item) => item.objectId),
      readCounts = new Map<string, number>();
    if (ids.length) {
      const reads = await client.request(
        "objects.query",
        {
          contractKey: "task-board/comment-read-state",
          contractVersions: ["1.0.0"],
          where: and(
            rootWhere(props.workspace.rootObjectId),
            {
              op: "eq",
              field: "data:/principalId",
              value: props.workspace.callerPrincipalId,
            },
            { op: "in", field: "data:/taskId", value: ids },
          ),
          select: ["data:/taskId", "data:/readAgentCommentCount"],
          limit: 100,
        },
        { signal },
      );
      for (const item of reads.items)
        readCounts.set(
          text(item.values["data:/taskId"]),
          Number(item.values["data:/readAgentCommentCount"] ?? 0),
        );
    }
    return {
      nextCursor: result.nextCursor,
      items: result.items.map((item) => {
        const workflowState = text(
            item.values["data:/workflowState"],
          ) as TaskBoard.Task["workflowState"],
          waiting = text(item.values["data:/waiting/reason"]);
        const waitingDetail = text(item.values["data:/waiting/detail"]),
          claimPhase = text(item.values["data:/claim/phase"]),
          resolvedHost = text(item.values["data:/claim/target/hostId"]);
        const requirementKind = text(
          item.values["data:/fields/executionRequirement/kind"],
        );
        const requestedEnvironment =
          requirementKind === "host"
            ? text(item.values["data:/fields/executionRequirement/hostId"])
            : "";
        const requirement =
          requirementKind === "host"
            ? { kind: "host" as const, hostId: requestedEnvironment }
            : null;
        const derived = executionCondition({
          workflowState,
          control: text(
            item.values["data:/fields/control"],
          ) as TaskBoard.TaskFields["control"],
          publication: !!item.values["data:/publication"],
          claim: claimPhase
            ? {
                phase: claimPhase as TaskBoard.Claim["phase"],
                hostId: resolvedHost,
              }
            : null,
          waiting: waiting
            ? {
                reason: waiting as TaskBoard.Waiting["reason"],
                detail: waitingDetail,
              }
            : null,
          executionRequirement: requirement,
        });
        return {
          id: item.objectId,
          taskKey: text(item.values["data:/taskKey"]),
          title: text(item.values["data:/fields/title"]),
          category: text(item.values["data:/fields/category"]),
          priority: item.values["data:/fields/priority"],
          control: text(
            item.values["data:/fields/control"],
          ) as TaskBoard.TaskFields["control"],
          requestedEnvironment,
          resolvedHost,
          condition: derived.state,
          conditionDetail: derived.detail,
          nextReviewAt: text(item.values["data:/fields/nextReviewAt"]),
          dueAt: text(item.values["data:/fields/dueAt"]),
          unreadAgentComments: Math.max(
            0,
            Number(item.values["data:/agentCommentCount"] ?? 0) -
              (readCounts.get(item.objectId) ?? 0),
          ),
          context: {
            workflowState,
            control: text(
              item.values["data:/fields/control"],
            ) as TaskBoard.TaskFields["control"],
            claimed: !!claimPhase,
            waiting,
            hasResult: !!item.values["data:/latestResult"],
            publishing: !!item.values["data:/publication"],
          },
        };
      }),
    };
  }, 15000, () =>
    "ivy.task-board.column:" +
    base.href +
    ":" +
    props.workspace.serviceNodeId +
    ":" +
    props.workspace.callerPrincipalId +
    ":" +
    props.column.id +
    ":" +
    !!props.archived +
    ":" +
    props.swimlane +
    ":" +
    JSON.stringify(
      ["q", "status", "control", "host", "category", "project"].map((key) =>
        props.query.get(key),
      ),
    ), ["objects/task-board/task"]);
type Item = { id: string; category: string; context: TransitionContext };
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
const items = computed(() => (pages.value.value?.items ?? []) as Item[]);
// Neighbours within the same swimlane, so ranking never jumps across categories.
const sameLane = (a: Item, b: Item | undefined) =>
  !!b && (props.swimlane !== "category" || lane(a.category) === lane(b.category));
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
const movable = (item: { context: TransitionContext }) =>
  !props.archived &&
  !props.disabled &&
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
      <DropdownMenu v-if="column.id === 'done' && workspace.role === 'user'">
        <DropdownMenuTrigger as-child>
          <Button variant="ghost" size="icon-sm" class="ml-auto" aria-label="Done actions" :disabled="disabled">
            <MoreHorizontal aria-hidden="true" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem :disabled="disabled || !count" @select="emit('archiveAll')">
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
      <template
        v-for="(task, index) in pages.value.value?.items ?? []"
        :key="task.id"
        ><h3
          v-if="
            swimlane === 'category' &&
            !archived &&
            (index === 0 ||
              lane(task.category) !==
                lane(pages.value.value!.items[index - 1]!.category))
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
              :title="'Review ' + new Date(task.nextReviewAt).toLocaleString()"
              ><Clock class="size-3.5" aria-hidden="true" />{{
                shortDate(task.nextReviewAt)
              }}</span
            >
            <span
              v-if="task.unreadAgentComments"
              class="inline-flex items-center gap-1 font-medium text-foreground"
              :aria-label="
                task.unreadAgentComments +
                ' unread agent comment' +
                (task.unreadAgentComments === 1 ? '' : 's')
              "
              ><MessageSquare class="size-3.5" aria-hidden="true" />{{
                task.unreadAgentComments
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
            v-if="noteworthyCondition(task.condition)"
            class="relative"
            :class="layout === 'list' ? 'shrink-0' : 'mt-2'"
            :label="conditionLabel(task.condition)"
            :tone="conditionTone(task.condition)"
            :title="task.conditionDetail"
          />
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
                :disabled="disabled"
                @select="emit('transition', task.id, item.action)"
                >{{ item.label }}</DropdownMenuItem
              ><template v-if="ranked"
                ><DropdownMenuSeparator /><DropdownMenuItem
                  v-if="previous(index) || pages.page.value > 1"
                  :disabled="disabled"
                  @select="emit('reorder', task.id, { top: true })"
                  >Move to top</DropdownMenuItem
                ><DropdownMenuItem
                  v-if="previous(index)"
                  :disabled="disabled"
                  @select="emit('reorder', task.id, { before: previous(index)!.id })"
                  >Move up</DropdownMenuItem
                ><DropdownMenuItem
                  v-if="next(index)"
                  :disabled="disabled"
                  @select="emit('reorder', task.id, { after: next(index)!.id })"
                  >Move down</DropdownMenuItem
                ><DropdownMenuItem
                  v-if="moreBelow(index)"
                  :disabled="disabled"
                  @select="emit('reorder', task.id, { before: null })"
                  >Move to bottom</DropdownMenuItem
                ></template
              ><template v-if="!archived && workspace.role === 'user' && ['done', 'cancelled'].includes(task.context.workflowState)">
                <DropdownMenuSeparator />
                <DropdownMenuItem :disabled="disabled || task.context.claimed || task.context.publishing" @select="emit('archive', task.id)">
                  <Archive aria-hidden="true" />Archive task
                </DropdownMenuItem>
              </template></DropdownMenuContent
            ></DropdownMenu
          >
        </article></template
      >
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
