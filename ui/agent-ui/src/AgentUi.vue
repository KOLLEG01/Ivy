<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import {
  Button,
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  HostMark,
  Kbd,
  LoadingIndicator,
  SearchDialog,
  SearchResult,
  SidebarGroup,
  SidebarGroupAction,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarMenu,
  SidebarMenuAction,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSub,
  useRemote,
} from "@ivy/ui";
import {
  ChevronRight,
  ChevronsUpDown,
  Folder,
  FolderOpen,
  ListFilter,
  MoreHorizontal,
  Plus,
  Search,
  SquarePen,
} from "@lucide/vue";
import type {
  Agent,
  Operation,
  Transport,
} from "../../../packages/sdk/src/client.js";
import {
  serviceNodeLabel,
  record,
  text,
} from "../../../packages/ui-client/src/native";
import UiFrame from "../../../packages/ui-client/src/UiFrame.vue";
import {
  route,
  useHashRoute,
  usePage,
} from "../../../packages/ui-client/src/runtime";
import { base, client, notifications } from "./runtime";
import HostUsage from "./HostUsage.vue";
import TaskRow from "./TaskRow.vue";
import AgentHosts from "./AgentHosts.vue";
import AgentHost from "./AgentHost.vue";
import AgentHostSettings from "./AgentHostSettings.vue";
import AgentSettings from "./AgentSettings.vue";
import AgentTask from "./AgentTask.vue";
import ProjectActions from "../../../packages/ui-client/src/ProjectActions.vue";
import { hostSections, isVisibleTask, partitionInternalTasks } from "./task-groups";
import type { ScopedProject } from "./task-groups";
import {
  liveWorking,
  markSeen,
  readSeen,
  taskActivity,
  taskKey,
} from "./task-activity";
import type { LiveTaskState, SeenTasks } from "./task-activity";

const hash = useHashRoute();
const section = computed(() => hash.value.slice(2).split("?")[0] || "home");
const query = computed(
  () => new URLSearchParams(hash.value.split("?")[1] ?? ""),
);
const node = computed(() => query.value.get("node") ?? "");
// The task tree always spans every host; a host filter only narrows it and never follows navigation.
const filterKey = "ivy.agent.host-filter:" + base.href,
  internalKey = "ivy.agent.show-internal:" + base.href;
const hostFilter = ref(""),
  showInternal = ref(false);
try {
  hostFilter.value = sessionStorage.getItem(filterKey) ?? "";
  showInternal.value = localStorage.getItem(internalKey) === "true";
} catch {
  /* Optional view state. */
}
const filterHost = (value: unknown) => {
  hostFilter.value = value === "all" ? "" : String(value);
  try {
    sessionStorage.setItem(filterKey, hostFilter.value);
  } catch {
    /* Optional view state. */
  }
};
const toggleInternal = (value: boolean) => {
  showInternal.value = value;
  try {
    localStorage.setItem(internalKey, String(value));
  } catch {
    /* Optional view state. */
  }
};
// The project tree is the default; the activity view lists every task by its latest update.
const viewKey = "ivy.agent.view:" + base.href;
const view = ref<"projects" | "activity">("projects");
try {
  if (localStorage.getItem(viewKey) === "activity") view.value = "activity";
} catch {
  /* Optional view state. */
}
const chooseView = (value: unknown) => {
  view.value = value === "activity" ? "activity" : "projects";
  try {
    localStorage.setItem(viewKey, view.value);
  } catch {
    /* Optional view state. */
  }
};
const search = ref("");
const searchOpen = ref(false);
watch(searchOpen, (open) => {
  if (!open) search.value = "";
});

const nodes = usePage((signal, cursor) =>
    client.request(
      "serviceNodes.list",
      {
        serviceName: "agent-manager",
        limit: 50,
        ...(cursor ? { cursor } : {}),
      },
      { signal },
    ), 30000, undefined, ["services/agent-manager"]);
// Every unarchived task stays listed; archiving or deleting is how a user tidies the list.
const recent = useRemote(async (signal) => {
  const items: Operation.InventoryItem[] = [];
  let cursor: string | undefined;
  do {
    const page = await client.request(
      "inventory.list",
      {
        ...(hostFilter.value
          ? { serviceNodeId: hostFilter.value }
          : { serviceName: "agent-manager" }),
        namespace: "codex",
        kind: "thread",
        archived: false,
        sort: "recency-desc",
        ...(search.value ? { searchTerm: search.value } : {}),
        limit: 200,
        ...(cursor ? { cursor } : {}),
      },
      { signal },
    );
    items.push(...page.items.filter(isVisibleTask));
    cursor = page.nextCursor ?? undefined;
  } while (cursor);
  return { items };
}, 30000, () => ["inventory/codex/thread" + (hostFilter.value ? "/" + hostFilter.value : "")]);
const allProjects = useRemote(async (signal) => {
  const result: ScopedProject[] = [];
  let cursor: string | undefined;
  do {
    const page = await client.request("inventory.list", { serviceName: "agent-manager", namespace: "codex", kind: "project", limit: 200,
      ...(cursor ? { cursor } : {}) }, { signal });
    for (const item of page.items) {
      const project = record(item.summary);
      if (project.source === "native" && typeof project.nativeId === "string" && typeof project.name === "string" && Array.isArray(project.paths))
        result.push({ serviceNodeId: item.resourceRef.serviceNodeId, project: project as Agent.ProjectSummary });
    }
    cursor = page.nextCursor ?? undefined;
  } while (cursor);
  return result;
}, 30000, ["inventory/codex/project"]);

const lastNode = ref("");
try {
  lastNode.value =
    sessionStorage.getItem("ivy.agent.last-node:" + base.href) ?? "";
} catch {
  /* Optional view state. */
}
watch(hostFilter, () => {
  recent.value.value = null;
  void recent.refresh();
});
watch(node, (value) => {
  if (value) {
    lastNode.value = value;
    try {
      sessionStorage.setItem("ivy.agent.last-node:" + base.href, value);
    } catch {
      /* Optional view state. */
    }
  }
});

const fallbackNode = computed(() => {
  const available = nodes.value.value?.items ?? [];
  return (
    available.find((item) => item.serviceNodeId === lastNode.value)
      ?.serviceNodeId ??
    available.find((item) => item.ready && item.connected)?.serviceNodeId ??
    available[0]?.serviceNodeId ??
    ""
  );
});
watch(
  [section, fallbackNode],
  ([current, value]) => {
    if (current === "home" && value)
      window.location.hash = route("host", { node: value });
  },
  { immediate: true },
);
const managementNode = computed(() => {
  const available = nodes.value.value?.items ?? [];
  return (
    available.find((item) => item.ready && item.connected)?.serviceNodeId ??
    available.find((item) => item.connected)?.serviceNodeId ??
    available[0]?.serviceNodeId ??
    ""
  );
});
const hostName = (serviceNodeId: string) =>
  serviceNodeLabel(nodes.value.value?.items ?? [], serviceNodeId);
const taskName = (summary: unknown, id: string) =>
  text(record(summary).name) ||
  text(record(summary).preview).slice(0, 100) ||
  id;
const hostItem = (serviceNodeId: string) =>
  nodes.value.value?.items.find((item) => item.serviceNodeId === serviceNodeId);
const hostReady = (serviceNodeId: string) => {
  const item = hostItem(serviceNodeId);
  return !!item?.ready && !!item.connected;
};
const scopedProjects = computed(() =>
  (allProjects.value.value ?? []).filter(value => !hostFilter.value || value.serviceNodeId === hostFilter.value));
const projectOf = (serviceNodeId: string, id: string) =>
  scopedProjects.value.find(value => value.serviceNodeId === serviceNodeId && value.project.nativeId === id)?.project;
// Tasks wait for the project catalog so internal work never flashes into a user project.
const tasks = computed(() => allProjects.value.value === null ? [] : recent.value.value?.items ?? []);
const taskLists = computed(() => partitionInternalTasks(scopedProjects.value, tasks.value));
const visibleRecent = computed(() => taskLists.value.recent);
const sections = computed(() =>
  hostSections(
    hostFilter.value ? [hostFilter.value] : (nodes.value.value?.items ?? []).map(item => item.serviceNodeId),
    scopedProjects.value,
    tasks.value,
  ).map(value => ({ ...value, groups: value.groups.filter(group => !search.value || group.tasks.length) }))
    .filter(value => !search.value || value.groups.length || (showInternal.value && value.internal.length)),
);
const internalOpen = ref(new Set<string>());
const toggleInternalGroup = (serviceNodeId: string) => {
  const next = new Set(internalOpen.value);
  if (!next.delete(serviceNodeId)) next.add(serviceNodeId);
  internalOpen.value = next;
};

let searchTimer: ReturnType<typeof setTimeout> | null = null;
watch(search, () => {
  if (searchTimer) clearTimeout(searchTimer);
  searchTimer = setTimeout(() => {
    recent.value.value = null;
    void recent.refresh();
  }, 250);
});
// Lifecycle events mark tasks working or finished immediately; the inventory reread that follows
// confirms them once AgentManager has published its new snapshot.
const seenKey = "ivy.agent.seen:" + base.href;
let storedSeen: string | null = null;
try {
  storedSeen = localStorage.getItem(seenKey);
} catch {
  /* Optional view state. */
}
const seen = ref<SeenTasks>(readSeen(storedSeen));
const live = ref(new Map<string, LiveTaskState>());
const clock = ref(Date.now());
const trackedTasks = computed(() => new Map(visibleRecent.value.map(item => [
  taskKey(item.resourceRef.serviceNodeId, item.resourceRef.nativeId), item,
])));
// Remove old internal badge state too, including tasks moved into IvyInternal.
watch(() => taskLists.value.internal, (items) => {
  const tasks = { ...seen.value.tasks }, working = new Map(live.value);
  let changed = false;
  for (const item of items) {
    const key = taskKey(item.resourceRef.serviceNodeId, item.resourceRef.nativeId);
    working.delete(key);
    if (key in tasks) { delete tasks[key]; changed = true; }
  }
  live.value = working;
  if (changed) {
    seen.value = { ...seen.value, tasks };
    try { localStorage.setItem(seenKey, JSON.stringify(seen.value)); } catch { /* Optional view state. */ }
  }
}, { immediate: true });
const openKey = computed(() =>
  section.value === "task" && node.value && query.value.get("id")
    ? taskKey(node.value, query.value.get("id")!)
    : "",
);
const seeTask = (key: string) => {
  if (!key) return;
  const item = trackedTasks.value.get(key);
  if (!item) return;
  seen.value = markSeen(seen.value, key, record(item.summary).updatedAt);
  try {
    localStorage.setItem(seenKey, JSON.stringify(seen.value));
  } catch {
    /* Optional view state. */
  }
};
// The open task is read on arrival, while it updates and when it is left.
watch(openKey, (value, previous) => {
  seeTask(previous ?? "");
  seeTask(value);
});
watch(
  trackedTasks,
  () => seeTask(openKey.value),
  {
    immediate: true,
  },
);
const activityOf = (item: Operation.InventoryItem) =>
  taskActivity(item, live.value, seen.value, openKey.value, clock.value);
// A live lifecycle event counts as an update before the inventory reports it.
const updatedAt = (item: Operation.InventoryItem) => Math.max(
  (Number(record(item.summary).updatedAt) || 0) * 1000,
  live.value.get(taskKey(item.resourceRef.serviceNodeId, item.resourceRef.nativeId))?.at ?? 0,
);
const activityTasks = computed(() =>
  [...(showInternal.value ? tasks.value : visibleRecent.value)].sort((a, b) => updatedAt(b) - updatedAt(a)));
const ageUnits = [["minute", 60], ["hour", 3600], ["day", 86400]] as const;
const ageFormats = new Map(ageUnits.map(([unit]) => [unit, new Intl.NumberFormat(undefined, { style: "unit", unit, unitDisplay: "narrow" })]));
const age = (item: Operation.InventoryItem) => {
  const at = updatedAt(item), seconds = (clock.value - at) / 1000;
  if (!at) return "";
  if (seconds < 60) return ageFormats.get("minute")!.format(0);
  if (seconds >= 7 * 86400) return new Date(at).toLocaleDateString([], { day: "numeric", month: "short" });
  const [unit, size] = [...ageUnits].reverse().find(([, size]) => seconds >= size)!;
  return ageFormats.get(unit)!.format(Math.floor(seconds / size));
};
const taskChanged = (value: Transport.ProviderNotification) => {
  const payload = record(value.params.payload),
    method = text(payload.method),
    threadId = text(record(payload.params).threadId);
  const working = liveWorking(method, payload.params);
  if (threadId && working !== undefined) {
    const key = taskKey(value.params.serviceNodeId, threadId);
    if (!trackedTasks.value.has(key)) return;
    live.value = new Map(live.value).set(key, { working, at: Date.now() });
    clock.value = Date.now();
    if (key === openKey.value) seeTask(key);
  }
};
const unsubscribe = notifications.subscribe(
  { namespace: "agent", name: "notification", version: "1.0.0" },
  taskChanged,
);
const ticker = setInterval(() => {
  if (live.value.size || view.value === "activity") clock.value = Date.now();
}, 30000);
watch(view, () => { clock.value = Date.now(); });
onBeforeUnmount(() => {
  if (searchTimer) clearTimeout(searchTimer);
  clearInterval(ticker);
  unsubscribe();
});

const collapsed = ref(new Set<string>());
try {
  const value = JSON.parse(
    sessionStorage.getItem("ivy.agent.projects:" + base.href) ?? "[]",
  );
  if (Array.isArray(value))
    collapsed.value = new Set(value.filter((id) => typeof id === "string"));
} catch {
  /* Optional view state. */
}
const toggleGroup = (id: string) => {
  if (collapsed.value.has(id)) collapsed.value.delete(id);
  else collapsed.value.add(id);
  try {
    sessionStorage.setItem(
      "ivy.agent.projects:" + base.href,
      JSON.stringify([...collapsed.value]),
    );
  } catch {
    /* Optional view state. */
  }
};
</script>

<template>
  <UiFrame
    ui-name="Agents"
    layout="conversation"
    :settings-href="node ? route('host-settings', { node }) : route('settings')"
    :base="base"
    :client="client"
    :navigation="[]"
  >
    <template #sidebar-actions>
      <SidebarGroup class="pb-0">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton v-if="fallbackNode" as-child
              ><a :href="route('host', { node: hostFilter || node || fallbackNode })"
                ><SquarePen aria-hidden="true" /><span>New task</span></a
              ></SidebarMenuButton
            >
            <SidebarMenuButton v-else disabled
              ><SquarePen aria-hidden="true" /><span
                >New task</span
              ></SidebarMenuButton
            >
          </SidebarMenuItem>
          <SidebarMenuItem>
            <SidebarMenuButton @click="searchOpen = true"
              ><Search aria-hidden="true" /><span>Search tasks</span
              ><Kbd class="ml-auto">Ctrl K</Kbd></SidebarMenuButton
            >
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarGroup>
    </template>
    <template #sidebar>
      <nav class="agent-sidebar" aria-label="Recent tasks">
        <SidebarGroup class="pb-0">
          <SidebarMenu>
            <SidebarMenuItem>
              <DropdownMenu>
                <DropdownMenuTrigger as-child>
                  <SidebarMenuButton aria-label="Filter tasks">
                    <ListFilter aria-hidden="true" :class="{ 'text-primary': hostFilter }" />
                    <span class="truncate">{{ hostFilter ? hostName(hostFilter) : "All hosts" }}</span>
                    <ChevronsUpDown class="ml-auto" aria-hidden="true" />
                  </SidebarMenuButton>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start" class="w-60">
                  <DropdownMenuLabel class="text-xs text-muted-foreground">Show tasks from</DropdownMenuLabel>
                  <DropdownMenuRadioGroup :model-value="hostFilter || 'all'" @update:model-value="filterHost">
                    <DropdownMenuRadioItem value="all">All hosts</DropdownMenuRadioItem>
                    <DropdownMenuRadioItem
                      v-for="item in nodes.value.value?.items"
                      :key="item.serviceNodeId"
                      :value="item.serviceNodeId"
                    >
                      <HostMark :label="hostName(item.serviceNodeId)" :seed="item.serviceNodeId" />
                      <span class="truncate">{{ hostName(item.serviceNodeId) }}</span>
                      <span
                        v-if="!hostReady(item.serviceNodeId)"
                        class="ml-auto size-2 shrink-0 rounded-full bg-amber-500"
                        role="img"
                        aria-label="Not ready"
                      />
                    </DropdownMenuRadioItem>
                  </DropdownMenuRadioGroup>
                  <p
                    v-if="nodes.value.value?.items.length === 0"
                    class="px-2 py-1.5 text-sm text-muted-foreground"
                  >
                    No AgentManager is available.
                  </p>
                  <DropdownMenuSeparator />
                  <DropdownMenuCheckboxItem
                    :model-value="showInternal"
                    @update:model-value="toggleInternal($event === true)"
                    >Show IvyInternal</DropdownMenuCheckboxItem
                  >
                  <DropdownMenuSeparator />
                  <DropdownMenuLabel class="text-xs text-muted-foreground">Organize</DropdownMenuLabel>
                  <DropdownMenuRadioGroup :model-value="view" @update:model-value="chooseView">
                    <DropdownMenuRadioItem value="projects">By project</DropdownMenuRadioItem>
                    <DropdownMenuRadioItem value="activity">By latest update</DropdownMenuRadioItem>
                  </DropdownMenuRadioGroup>
                </DropdownMenuContent>
              </DropdownMenu>
            </SidebarMenuItem>
          </SidebarMenu>
        </SidebarGroup>
        <LoadingIndicator
          v-if="!tasks.length && (recent.loading.value || allProjects.loading.value) && !(recent.error.value || allProjects.error.value)"
          label="Loading tasks…"
        />
        <SidebarGroup v-if="view === 'activity'">
          <SidebarGroupContent>
            <SidebarMenu>
              <TaskRow
                v-for="item in activityTasks"
                :key="taskKey(item.resourceRef.serviceNodeId, item.resourceRef.nativeId)"
                :item="item"
                :name="taskName(item.summary, item.resourceRef.nativeId)"
                :active="openKey === taskKey(item.resourceRef.serviceNodeId, item.resourceRef.nativeId)"
                :activity="activityOf(item)"
                :host="hostName(item.resourceRef.serviceNodeId)"
                :age="age(item)"
                @changed="recent.refresh()"
              />
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
        <template v-else>
          <Collapsible
            v-for="host in sections"
            :key="host.serviceNodeId"
            as-child
            :open="!collapsed.has('host:' + host.serviceNodeId)"
            @update:open="toggleGroup('host:' + host.serviceNodeId)"
          >
            <SidebarGroup>
              <SidebarGroupLabel class="gap-1.5 pr-8">
                <HostUsage
                  :node="host.serviceNodeId"
                  :label="hostName(host.serviceNodeId)"
                  :ready="hostReady(host.serviceNodeId)"
                >
                  <button
                    type="button"
                    class="-m-0.5 flex shrink-0 rounded-[5px] p-0.5 outline-hidden ring-sidebar-ring hover:bg-sidebar-accent focus-visible:ring-2"
                    :aria-label="'Usage on ' + hostName(host.serviceNodeId)"
                    :title="'Usage on ' + hostName(host.serviceNodeId)"
                  >
                    <HostMark :label="hostName(host.serviceNodeId)" :seed="host.serviceNodeId" />
                  </button>
                </HostUsage>
                <CollapsibleTrigger
                  class="group/host flex h-full min-w-0 flex-1 items-center gap-1.5 rounded-md text-left outline-hidden ring-sidebar-ring hover:text-sidebar-foreground focus-visible:ring-2"
                  :title="hostName(host.serviceNodeId)"
                >
                  <span class="truncate">{{ hostName(host.serviceNodeId) }}</span>
                  <span
                    v-if="!hostReady(host.serviceNodeId)"
                    class="size-1.5 shrink-0 rounded-full bg-amber-500"
                    role="img"
                    aria-label="Not ready"
                  />
                  <ChevronRight
                    class="size-3.5 shrink-0 opacity-60 transition-transform group-data-[state=open]/host:rotate-90"
                    aria-hidden="true"
                  />
                </CollapsibleTrigger>
              </SidebarGroupLabel>
              <SidebarGroupAction as-child>
                <a
                  :href="route('host', { node: host.serviceNodeId })"
                  :aria-label="'New task on ' + hostName(host.serviceNodeId)"
                  :title="'New task on ' + hostName(host.serviceNodeId)"
                  ><Plus aria-hidden="true"
                /></a>
              </SidebarGroupAction>
              <CollapsibleContent>
                <SidebarGroupContent>
                  <SidebarMenu>
                    <Collapsible
                      v-for="group in host.groups"
                      :key="group.id"
                      as-child
                      :open="!collapsed.has(host.serviceNodeId + ':' + group.id)"
                      @update:open="toggleGroup(host.serviceNodeId + ':' + group.id)"
                    >
                      <SidebarMenuItem>
                        <CollapsibleTrigger as-child>
                          <SidebarMenuButton :title="group.name"
                            ><Folder
                              v-if="collapsed.has(host.serviceNodeId + ':' + group.id)"
                              aria-hidden="true"
                            /><FolderOpen v-else aria-hidden="true" /><span
                              class="truncate"
                              >{{ group.name }}</span
                            ></SidebarMenuButton
                          >
                        </CollapsibleTrigger>
                        <SidebarMenuAction v-if="group.path" as-child show-on-hover class="right-7"
                          ><a
                            :href="
                              route('host', {
                                node: host.serviceNodeId,
                                project: group.path,
                                projectId: group.id,
                              })
                            "
                            :aria-label="'New task in ' + group.name"
                            ><Plus aria-hidden="true" /></a
                        ></SidebarMenuAction>
                        <ProjectActions v-if="group.path && projectOf(host.serviceNodeId, group.id)" :client="client" :node="host.serviceNodeId"
                          :project="projectOf(host.serviceNodeId, group.id)!"
                          @changed="allProjects.refresh(); recent.refresh()">
                          <SidebarMenuAction :aria-label="'Manage project ' + group.name" show-on-hover><MoreHorizontal aria-hidden="true" /></SidebarMenuAction>
                        </ProjectActions>
                        <CollapsibleContent>
                          <SidebarMenuSub>
                            <TaskRow
                              v-for="item in group.tasks"
                              :key="item.resourceRef.nativeId"
                              nested
                              :item="item"
                              :name="taskName(item.summary, item.resourceRef.nativeId)"
                              :active="openKey === taskKey(host.serviceNodeId, item.resourceRef.nativeId)"
                              :activity="activityOf(item)"
                              @changed="recent.refresh()"
                            />
                            <li v-if="!group.tasks.length" class="px-2 py-1 text-xs text-muted-foreground">No tasks</li>
                          </SidebarMenuSub>
                        </CollapsibleContent>
                      </SidebarMenuItem>
                    </Collapsible>
                    <Collapsible
                      v-if="showInternal && host.hasInternal"
                      as-child
                      :open="internalOpen.has(host.serviceNodeId)"
                      @update:open="toggleInternalGroup(host.serviceNodeId)"
                    >
                      <SidebarMenuItem>
                        <CollapsibleTrigger as-child>
                          <SidebarMenuButton title="IvyInternal">
                            <FolderOpen v-if="internalOpen.has(host.serviceNodeId)" aria-hidden="true" /><Folder v-else aria-hidden="true" />
                            <span>IvyInternal</span>
                          </SidebarMenuButton>
                        </CollapsibleTrigger>
                        <SidebarMenuBadge>{{ host.internal.length }}</SidebarMenuBadge>
                        <CollapsibleContent>
                          <SidebarMenuSub>
                            <TaskRow
                              v-for="item in host.internal"
                              :key="item.resourceRef.nativeId"
                              nested
                              :item="item"
                              :name="taskName(item.summary, item.resourceRef.nativeId)"
                              :active="openKey === taskKey(host.serviceNodeId, item.resourceRef.nativeId)"
                              :activity="activityOf(item)"
                              @changed="recent.refresh()"
                            />
                            <li v-if="!host.internal.length" class="px-2 py-1 text-xs text-muted-foreground">No tasks</li>
                          </SidebarMenuSub>
                        </CollapsibleContent>
                      </SidebarMenuItem>
                    </Collapsible>
                    <li
                      v-if="!host.groups.length && !(showInternal && host.hasInternal)"
                      class="px-2 py-1 text-xs text-muted-foreground"
                    >
                      No tasks
                    </li>
                  </SidebarMenu>
                </SidebarGroupContent>
              </CollapsibleContent>
            </SidebarGroup>
          </Collapsible>
        </template>
        <p
          v-if="recent.value.value?.items.length === 0"
          class="px-4 py-2 text-xs text-muted-foreground"
        >
          No matching tasks
        </p>
        <p
          v-if="recent.error.value || allProjects.error.value || nodes.error.value"
          class="px-4 text-xs text-muted-foreground"
        >
          Task navigation unavailable.
          <Button
            variant="link"
            size="sm"
            class="h-8 px-0 text-xs"
            @click="
              recent.refresh();
              allProjects.refresh();
              nodes.refresh();
            "
          >
            Retry
          </Button>
        </p>
      </nav>
    </template>
    <AgentTask
      v-if="section === 'task' && node && query.get('id')"
      :key="node + ':' + query.get('id')"
      :node="node"
      :thread-id="query.get('id')!"
      :turn-id="query.get('turn') ?? ''"
    />
    <AgentHostSettings
      v-else-if="section === 'host-settings' && node"
      :key="node"
      :node="node"
    />
    <AgentSettings v-else-if="section === 'settings'" :node="managementNode" />
    <AgentHost
      v-else-if="section === 'host' && node"
      :key="hash"
      :node="node"
      :project="query.get('project') ?? ''"
      :project-id="query.get('projectId') ?? ''"
    />
    <AgentHost
      v-else-if="section === 'home' && fallbackNode"
      :key="fallbackNode"
      :node="fallbackNode"
      project=""
      project-id=""
    />
    <AgentHosts
      v-else
      :nodes="nodes.value.value?.items ?? []"
      :tasks="visibleRecent"
      :loading="nodes.loading.value || recent.loading.value"
      :error="nodes.error.value || recent.error.value"
      @refresh="
        nodes.refresh();
        recent.refresh();
      "
    />
  </UiFrame>
  <SearchDialog
    v-model:open="searchOpen"
    v-model:query="search"
    title="Search tasks"
    label="Find a task"
    placeholder="Search tasks"
    :loading="recent.loading.value"
    :empty="!recent.value.value?.items.length"
    empty-text="No matching tasks"
  >
    <SearchResult
      v-for="item in recent.value.value?.items"
      :key="item.resourceRef.serviceNodeId + ':' + item.resourceRef.nativeId"
      :href="
        route('task', {
          node: item.resourceRef.serviceNodeId,
          id: item.resourceRef.nativeId,
        })
      "
      :title="taskName(item.summary, item.resourceRef.nativeId)"
      :detail="hostName(item.resourceRef.serviceNodeId)"
    />
  </SearchDialog>
</template>

<style scoped>
.agent-sidebar {
  min-width: 0;
  overflow-x: hidden;
}
</style>
