<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import {
  ActivityIndicator,
  Button,
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Kbd,
  SearchDialog,
  SearchResult,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarMenu,
  SidebarMenuAction,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSub,
  SidebarMenuSubButton,
  SidebarMenuSubItem,
  useRemote,
} from "@ivy/ui";
import {
  Check,
  ChevronsUpDown,
  Folder,
  FolderOpen,
  MoreHorizontal,
  Plus,
  Search,
  Server,
  SquarePen,
} from "@lucide/vue";
import type {
  Agent,
  Operation,
  Transport,
} from "../../../packages/sdk/src/client.js";
import {
  nativeRead,
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
import AgentHosts from "./AgentHosts.vue";
import AgentHost from "./AgentHost.vue";
import AgentHostSettings from "./AgentHostSettings.vue";
import AgentSettings from "./AgentSettings.vue";
import AgentTask from "./AgentTask.vue";
import ProjectActions from "../../../packages/ui-client/src/ProjectActions.vue";
import { groupTasks, isVisibleTask, isInternalProject, partitionInternalTasks } from "./task-groups";
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
    ), 30000, undefined, ["services"]);
const recent = usePage((signal, cursor) =>
    client.request(
      "inventory.list",
      {
        ...(node.value
          ? { serviceNodeId: node.value }
          : { serviceName: "agent-manager" }),
        namespace: "codex",
        kind: "thread",
        archived: false,
        sort: "recency-desc",
        ...(search.value ? { searchTerm: search.value } : {}),
        limit: 50,
        ...(cursor ? { cursor } : {}),
      },
      { signal },
    ).then((page) => ({ ...page, items: page.items.filter(isVisibleTask) })), 30000, undefined, ["inventory"]);
const projects = useRemote(async (signal) =>
  node.value
    ? ((await nativeRead(
        client,
        node.value,
        "agent.projects",
        {},
        signal,
      )) as Agent.ProjectsResult)
    : null,
  30000, ["inventory"],
);
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
}, 30000, ["inventory"]);

const lastNode = ref("");
try {
  lastNode.value =
    sessionStorage.getItem("ivy.agent.last-node:" + base.href) ?? "";
} catch {
  /* Optional view state. */
}
watch(node, (value) => {
  recent.value.value = null;
  recent.reset();
  projects.value.value = null;
  void projects.refresh();
  if (value) {
    lastNode.value = value;
    try {
      sessionStorage.setItem("ivy.agent.last-node:" + base.href, value);
    } catch {
      /* Optional view state. */
    }
  }
});

const selectedHost = computed(() =>
  nodes.value.value?.items.find((item) => item.serviceNodeId === node.value),
);
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
const scopedProjects = computed(() => node.value
  ? (projects.value.value?.projects ?? []).filter(project => project.source === "native").map(project => ({ serviceNodeId: node.value, project }))
  : allProjects.value.value ?? []);
const taskLists = computed(() => partitionInternalTasks(scopedProjects.value, recent.value.value?.items ?? []));
const visibleRecent = computed(() => allProjects.value.value === null ? [] : taskLists.value.recent);
const internalProjects = computed(() => scopedProjects.value.filter(value => isInternalProject(value.project)));
const internalOpen = ref(false);
const groups = computed(() =>
  groupTasks(
    scopedProjects.value.map(value => value.project).filter(project => !isInternalProject(project)),
    taskLists.value.recent,
  ).filter((group) => !search.value || group.tasks.length),
);

let searchTimer: ReturnType<typeof setTimeout> | null = null;
watch(search, () => {
  if (searchTimer) clearTimeout(searchTimer);
  searchTimer = setTimeout(() => {
    recent.value.value = null;
    recent.reset();
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
const openKey = computed(() =>
  section.value === "task" && node.value && query.value.get("id")
    ? taskKey(node.value, query.value.get("id")!)
    : "",
);
const seeTask = (key: string) => {
  if (!key) return;
  const item = recent.value.value?.items.find(
    (value) =>
      taskKey(value.resourceRef.serviceNodeId, value.resourceRef.nativeId) ===
      key,
  );
  seen.value = markSeen(seen.value, key, record(item?.summary).updatedAt);
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
  () => recent.value.value,
  () => seeTask(openKey.value),
  {
    immediate: true,
  },
);
const activityOf = (item: Operation.InventoryItem) =>
  taskActivity(item, live.value, seen.value, openKey.value, clock.value);
const taskChanged = (value: Transport.ProviderNotification) => {
  const payload = record(value.params.payload),
    method = text(payload.method),
    threadId = text(record(payload.params).threadId);
  const working = liveWorking(method, payload.params);
  if (threadId && working !== undefined) {
    const key = taskKey(value.params.serviceNodeId, threadId);
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
  clock.value = Date.now();
}, 30000);
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
              ><a :href="route('host', { node: node || fallbackNode })"
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
                  <SidebarMenuButton
                    variant="outline"
                    aria-label="Choose host scope"
                  >
                    <Server aria-hidden="true" />
                    <span class="truncate">{{
                      selectedHost ? hostName(selectedHost.serviceNodeId) : "All hosts"
                    }}</span>
                    <ChevronsUpDown class="ml-auto" aria-hidden="true" />
                  </SidebarMenuButton>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start" class="w-60">
                  <DropdownMenuLabel class="text-xs text-muted-foreground"
                    >Task scope</DropdownMenuLabel
                  >
                  <DropdownMenuItem as-child
                    ><a :href="route('hosts')"
                      ><span>All hosts</span
                      ><Check
                        v-if="!node"
                        class="ml-auto"
                        aria-hidden="true" /></a
                  ></DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    v-for="item in nodes.value.value?.items"
                    :key="item.serviceNodeId"
                    as-child
                  >
                    <a :href="route('host', { node: item.serviceNodeId })"
                      ><span
                        class="size-2 rounded-full"
                        :class="
                          item.ready && item.connected
                            ? 'bg-emerald-500'
                            : 'bg-amber-500'
                        "
                        aria-hidden="true" /><span class="truncate">{{
                        hostName(item.serviceNodeId)
                      }}</span
                      ><Check
                        v-if="node === item.serviceNodeId"
                        class="ml-auto"
                        aria-hidden="true"
                    /></a>
                  </DropdownMenuItem>
                  <p
                    v-if="nodes.value.value?.items.length === 0"
                    class="px-2 py-1.5 text-sm text-muted-foreground"
                  >
                    No AgentManager is available.
                  </p>
                </DropdownMenuContent>
              </DropdownMenu>
            </SidebarMenuItem>
          </SidebarMenu>
        </SidebarGroup>
        <SidebarGroup v-if="!node">
          <SidebarGroupLabel>Recent</SidebarGroupLabel>
          <SidebarGroupContent>
            <SidebarMenu>
              <SidebarMenuItem
                v-for="item in visibleRecent"
                :key="
                  item.resourceRef.serviceNodeId +
                  ':' +
                  item.resourceRef.nativeId
                "
              >
                <SidebarMenuButton
                  as-child
                  :class="{ 'pr-7': activityOf(item) }"
                  :is-active="query.get('id') === item.resourceRef.nativeId"
                  ><a
                    :href="
                      route('task', {
                        node: item.resourceRef.serviceNodeId,
                        id: item.resourceRef.nativeId,
                      })
                    "
                    :aria-current="
                      query.get('id') === item.resourceRef.nativeId
                        ? 'page'
                        : undefined
                    "
                    :title="
                      taskName(item.summary, item.resourceRef.nativeId) +
                      ' · ' +
                      hostName(item.resourceRef.serviceNodeId)
                    "
                    ><span>{{
                      taskName(item.summary, item.resourceRef.nativeId)
                    }}</span></a
                  ></SidebarMenuButton
                ><SidebarMenuBadge v-if="activityOf(item)"
                  ><ActivityIndicator :state="activityOf(item)!"
                /></SidebarMenuBadge>
              </SidebarMenuItem>
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
        <SidebarGroup v-else>
          <SidebarGroupLabel>Projects</SidebarGroupLabel>
          <SidebarGroupContent>
            <SidebarMenu>
              <Collapsible
                v-for="group in groups"
                :key="group.id"
                as-child
                :open="!collapsed.has(group.id)"
                @update:open="toggleGroup(group.id)"
              >
                <SidebarMenuItem>
                  <CollapsibleTrigger as-child>
                    <SidebarMenuButton :title="group.name"
                      ><Folder
                        v-if="collapsed.has(group.id)"
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
                          node,
                          project: group.path,
                          projectId: group.id,
                        })
                      "
                      :aria-label="'New task in ' + group.name"
                      ><Plus aria-hidden="true" /></a
                  ></SidebarMenuAction>
                  <ProjectActions v-if="group.path" :client="client" :node="node"
                    :project="projects.value.value!.projects.find(project => project.nativeId === group.id)!"
                    @changed="projects.refresh(); allProjects.refresh(); recent.refresh()">
                    <SidebarMenuAction :aria-label="'Manage project ' + group.name" show-on-hover><MoreHorizontal aria-hidden="true" /></SidebarMenuAction>
                  </ProjectActions>
                  <CollapsibleContent>
                    <SidebarMenuSub>
                      <SidebarMenuSubItem
                        v-for="item in group.tasks"
                        :key="item.resourceRef.nativeId"
                      >
                        <SidebarMenuSubButton
                          as-child
                          :class="{ 'pr-7': activityOf(item) }"
                          :is-active="
                            query.get('id') === item.resourceRef.nativeId
                          "
                          ><a
                            :href="
                              route('task', {
                                node,
                                id: item.resourceRef.nativeId,
                              })
                            "
                            :aria-current="
                              query.get('id') === item.resourceRef.nativeId
                                ? 'page'
                                : undefined
                            "
                            :title="
                              taskName(item.summary, item.resourceRef.nativeId)
                            "
                            ><span>{{
                              taskName(item.summary, item.resourceRef.nativeId)
                            }}</span></a
                          ></SidebarMenuSubButton
                        ><ActivityIndicator
                          v-if="activityOf(item)"
                          :state="activityOf(item)!"
                          class="pointer-events-none absolute top-1.5 right-1"
                        />
                      </SidebarMenuSubItem>
                      <li
                        v-if="!group.tasks.length"
                        class="px-2 py-1 text-xs text-muted-foreground"
                      >
                        No recent tasks
                      </li>
                    </SidebarMenuSub>
                  </CollapsibleContent>
                </SidebarMenuItem>
              </Collapsible>
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
        <SidebarGroup v-if="internalProjects.length">
          <SidebarGroupContent>
            <SidebarMenu>
              <Collapsible v-model:open="internalOpen" as-child>
                <SidebarMenuItem>
                  <CollapsibleTrigger as-child>
                    <SidebarMenuButton title="IvyInternal">
                      <FolderOpen v-if="internalOpen" aria-hidden="true" /><Folder v-else aria-hidden="true" />
                      <span>IvyInternal</span>
                    </SidebarMenuButton>
                  </CollapsibleTrigger>
                  <SidebarMenuBadge>{{ taskLists.internal.length }}</SidebarMenuBadge>
                  <CollapsibleContent>
                    <SidebarMenuSub>
                      <SidebarMenuSubItem v-for="item in taskLists.internal" :key="taskKey(item.resourceRef.serviceNodeId, item.resourceRef.nativeId)">
                        <SidebarMenuSubButton as-child :class="{ 'pr-7': activityOf(item) }"
                          :is-active="node === item.resourceRef.serviceNodeId && query.get('id') === item.resourceRef.nativeId">
                          <a :href="route('task', { node: item.resourceRef.serviceNodeId, id: item.resourceRef.nativeId })"
                            :title="taskName(item.summary, item.resourceRef.nativeId) + ' · ' + hostName(item.resourceRef.serviceNodeId)">
                            <span>{{ taskName(item.summary, item.resourceRef.nativeId) }}</span>
                          </a>
                        </SidebarMenuSubButton>
                        <ActivityIndicator v-if="activityOf(item)" :state="activityOf(item)!" class="pointer-events-none absolute top-1.5 right-1" />
                      </SidebarMenuSubItem>
                      <li v-if="!taskLists.internal.length" class="px-2 py-1 text-xs text-muted-foreground">No recent tasks</li>
                    </SidebarMenuSub>
                  </CollapsibleContent>
                </SidebarMenuItem>
              </Collapsible>
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
        <p
          v-if="recent.value.value?.items.length === 0"
          class="px-4 py-2 text-xs text-muted-foreground"
        >
          No matching tasks
        </p>
        <div
          v-if="recent.page.value > 1 || recent.value.value?.nextCursor"
          class="flex gap-1 px-2"
        >
          <Button
            v-if="recent.page.value > 1"
            variant="ghost"
            size="sm"
            :disabled="recent.loading.value"
            @click="recent.previous"
            >Newer</Button
          >
          <Button
            v-if="recent.value.value?.nextCursor"
            variant="ghost"
            size="sm"
            :disabled="recent.loading.value"
            @click="recent.next"
            >Older</Button
          >
        </div>
        <p
          v-if="recent.error.value || projects.error.value || allProjects.error.value || nodes.error.value"
          class="px-4 text-xs text-muted-foreground"
        >
          Task navigation unavailable.
          <Button
            variant="link"
            size="sm"
            class="h-8 px-0 text-xs"
            @click="
              recent.refresh();
              projects.refresh();
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
