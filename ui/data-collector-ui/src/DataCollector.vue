<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { Funnel, Plus } from "@lucide/vue";
import {
  ActivityIndicator,
  Alert,
  AlertDescription,
  Button,
  Card,
  CardDescription,
  CardHeader,
  CardTitle,
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
  OptionSelect,
  PageHeader,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarMenu,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
  StatusBadge,
} from "@ivy/ui";
import type { Operation } from "../../../packages/sdk/src/client.js";
import UiFrame from "../../../packages/ui-client/src/UiFrame.vue";
import { route, useHashRoute } from "../../../packages/ui-client/src/runtime";
import { base, client } from "./runtime";
import {
  collectorNodes,
  read,
  statusLabel,
  statusTone,
  tr,
  update,
  working,
} from "./collector";
import type { Task, TaskRow, TasksView } from "./collector";
import TaskEditor from "./TaskEditor.vue";
import TaskView from "./TaskView.vue";

const hash = useHashRoute();
const section = computed(() => hash.value.slice(2).split("?")[0] || "home");
const query = computed(
  () => new URLSearchParams(hash.value.split("?")[1] ?? ""),
);
const taskId = computed(() => query.value.get("id") ?? "");
const nodes = ref<Operation.ServiceNode[]>([]),
  node = ref(""),
  rows = ref<TaskRow[]>([]),
  limits = ref<TasksView["limits"]>(null),
  loaded = ref(false),
  busy = ref(false),
  error = ref("");
const current = computed(() =>
  rows.value.find((row) => row.task.id === taskId.value),
);
const editing = ref<{ task: Task; revision: number } | null>(null);

async function perform(work: () => Promise<void>) {
  busy.value = true;
  error.value = "";
  try {
    await work();
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    busy.value = false;
  }
}
async function refresh() {
  if (!node.value) return;
  const view = await read<TasksView>(node.value, { view: "tasks" });
  rows.value = view.items;
  limits.value = view.limits;
  loaded.value = true;
}
// The editor always starts from the stored task and its revision, never from list state.
watch(
  [section, taskId, node],
  ([name, id]) => {
    editing.value = null;
    if (name === "edit" && id && node.value)
      void perform(async () => {
        const full = await read<TaskRow & { task: Task }>(node.value, {
          view: "task",
          id,
        });
        editing.value = { task: full.task, revision: full.revision };
      });
  },
  { immediate: true },
);
const save = (task: Task) =>
  perform(async () => {
    await update(node.value, "save", {
      expectedRevision: editing.value?.revision ?? 0,
      task,
    });
    await refresh();
    window.location.hash = route("task", { id: task.id });
  });
const run = () =>
  perform(async () => {
    await update(node.value, "run", { id: taskId.value });
    await refresh();
  });
const toggle = () =>
  perform(async () => {
    const row = current.value!;
    await update(node.value, row.task.enabled ? "disable" : "enable", {
      id: row.task.id,
      expectedRevision: row.revision,
    });
    await refresh();
  });
const go = (target: string) => {
  window.location.hash = target;
};
const changeNode = () => {
  rows.value = [];
  loaded.value = false;
  window.location.hash = route("home");
  void perform(refresh);
};

// Runs finish in the background; poll quickly while one is active and slowly otherwise.
let timer: ReturnType<typeof setTimeout> | undefined,
  stopped = false;
const poll = () => {
  if (stopped) return;
  timer = setTimeout(
    async () => {
      if (!busy.value && !document.hidden)
        await refresh().catch(() => undefined);
      poll();
    },
    rows.value.some((row) => working(row.status)) ? 1500 : 5000,
  );
};
onMounted(() => {
  void perform(async () => {
    nodes.value = await collectorNodes();
    node.value =
      nodes.value.find((n) => n.ready && n.connected)?.serviceNodeId ??
      nodes.value[0]?.serviceNodeId ??
      "";
    await refresh();
  });
  poll();
});
onUnmounted(() => {
  stopped = true;
  clearTimeout(timer);
});
</script>

<template>
  <UiFrame
    ui-name="DataCollector"
    :base="base"
    :client="client"
    :navigation="[]"
  >
    <template #sidebar-actions>
      <SidebarGroup class="pb-0">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              as-child
              :is-active="section === 'new'"
              :disabled="!node"
            >
              <a :href="route('new')"
                ><Plus aria-hidden="true" /><span>{{
                  tr("Neuer Task", "New task")
                }}</span></a
              >
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>
      <SidebarGroup v-if="nodes.length > 1" class="pb-0">
        <SidebarGroupLabel>{{ tr("Dienst", "Service") }}</SidebarGroupLabel>
        <SidebarGroupContent class="px-2">
          <OptionSelect
            id="collector-node"
            v-model="node"
            class="w-full"
            :disabled="busy"
            @update:model-value="changeNode"
          >
            <option
              v-for="entry in nodes"
              :key="entry.serviceNodeId"
              :value="entry.serviceNodeId"
            >
              {{ entry.serviceNodeId }}
            </option>
          </OptionSelect>
        </SidebarGroupContent>
      </SidebarGroup>
    </template>
    <template #sidebar>
      <SidebarGroup>
        <SidebarGroupLabel>Tasks</SidebarGroupLabel>
        <SidebarGroupContent>
          <SidebarMenu>
            <SidebarMenuItem v-for="row in rows" :key="row.task.id">
              <SidebarMenuButton
                as-child
                :is-active="taskId === row.task.id"
                :class="{
                  'pr-7': working(row.status) || row.status === 'failed',
                }"
              >
                <a
                  :href="route('task', { id: row.task.id })"
                  :aria-current="taskId === row.task.id ? 'page' : undefined"
                  ><span
                    :class="{ 'text-muted-foreground': !row.task.enabled }"
                    >{{ row.task.name }}</span
                  ></a
                >
              </SidebarMenuButton>
              <SidebarMenuBadge v-if="working(row.status)"
                ><ActivityIndicator
                  state="working"
                  :label="statusLabel(row.status)"
              /></SidebarMenuBadge>
              <SidebarMenuBadge v-else-if="row.status === 'failed'"
                ><span
                  class="size-2 rounded-full bg-destructive"
                  role="img"
                  :aria-label="statusLabel(row.status)"
              /></SidebarMenuBadge>
            </SidebarMenuItem>
          </SidebarMenu>
          <p
            v-if="loaded && !rows.length"
            class="px-4 py-2 text-xs text-muted-foreground"
          >
            {{ tr("Noch keine Tasks", "No tasks yet") }}
          </p>
        </SidebarGroupContent>
      </SidebarGroup>
    </template>

    <Alert v-if="error" variant="destructive" class="mb-6"
      ><AlertDescription>{{ error }}</AlertDescription></Alert
    >

    <template v-if="section === 'new' || section === 'edit'">
      <PageHeader
        :title="
          section === 'new'
            ? tr('Neuer Task', 'New task')
            : tr('Task bearbeiten', 'Edit task')
        "
      />
      <TaskEditor
        v-if="node && (section === 'new' || editing)"
        :key="section + taskId + (editing?.revision ?? 0)"
        :task="section === 'new' ? null : editing!.task"
        :busy="busy"
        :limits="limits"
        @save="save"
        @cancel="
          go(section === 'new' ? route('home') : route('task', { id: taskId }))
        "
      />
    </template>
    <template v-else-if="section === 'task' && current">
      <PageHeader title="DataCollector" />
      <TaskView
        :key="node + current.task.id"
        :node="node"
        :row="current"
        :busy="busy"
        @edit="go(route('edit', { id: current.task.id }))"
        @run="run"
        @toggle="toggle"
      />
    </template>
    <template v-else>
      <PageHeader
        title="DataCollector"
        :description="
          tr(
            'Script-Tasks sammeln Daten und speichern versionierte Ergebnisse.',
            'Script tasks collect data and store versioned results.',
          )
        "
        :loading="busy"
      />
      <Empty v-if="!busy && !nodes.length" class="border border-dashed">
        <EmptyHeader>
          <EmptyMedia variant="icon"
            ><Funnel aria-hidden="true"
          /></EmptyMedia>
          <EmptyTitle>{{
            tr("Kein DataCollector-Dienst", "No DataCollector service")
          }}</EmptyTitle>
          <EmptyDescription>{{
            tr(
              "Kein DataCollector-Dienst registriert.",
              "No DataCollector service is registered.",
            )
          }}</EmptyDescription>
        </EmptyHeader>
      </Empty>
      <Empty v-else-if="loaded && !rows.length" class="border border-dashed">
        <EmptyHeader>
          <EmptyMedia variant="icon"
            ><Funnel aria-hidden="true"
          /></EmptyMedia>
          <EmptyTitle>{{ tr("Noch keine Tasks", "No tasks yet") }}</EmptyTitle>
          <EmptyDescription>{{
            tr(
              "Ein Task führt ein Script aus und speichert jedes Ergebnis als neue Revision.",
              "A task runs a script and stores every result as a new revision.",
            )
          }}</EmptyDescription>
        </EmptyHeader>
        <EmptyContent
          ><Button as-child
            ><a :href="route('new')"
              ><Plus aria-hidden="true" />{{ tr("Neuer Task", "New task") }}</a
            ></Button
          ></EmptyContent
        >
      </Empty>
      <p v-else-if="section === 'task' && loaded" class="text-muted-foreground">
        {{
          tr("Dieser Task existiert nicht mehr.", "This task no longer exists.")
        }}
      </p>
      <div v-else class="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
        <a
          v-for="row in rows"
          :key="row.task.id"
          :href="route('task', { id: row.task.id })"
          class="group rounded-xl outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
        >
          <Card
            class="h-full gap-3 transition-colors group-hover:border-foreground/20 group-hover:bg-accent/50"
          >
            <CardHeader>
              <div class="flex min-w-0 items-center justify-between gap-3">
                <CardTitle class="min-w-0 flex-1 truncate">{{ row.task.name }}</CardTitle>
                <StatusBadge
                  :label="statusLabel(row.status)"
                  :tone="statusTone(row.status)"
                />
              </div>
              <CardDescription
                >{{
                  row.task.enabled
                    ? tr("Aktiv", "Enabled")
                    : tr("Pausiert", "Disabled")
                }}
                ·
                {{
                  row.lastRunAt
                    ? tr("Zuletzt ", "Last run ") +
                      new Date(row.lastRunAt).toLocaleString()
                    : tr("Noch nicht gelaufen", "Not run yet")
                }}</CardDescription
              >
            </CardHeader>
          </Card>
        </a>
      </div>
    </template>
  </UiFrame>
</template>
