<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { useMediaQuery } from '@vueuse/core';
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
  RemoteState,
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
import HostUsage from "../../../packages/ui-client/src/HostUsage.vue";
import TaskRow from "./TaskRow.vue";
import AgentHosts from "./AgentHosts.vue";
import AgentHostAccounts from "./AgentHostAccounts.vue";
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
import { nativeHosts, nativeServiceNodes } from '../../../packages/ui-client/src/native-hosts';
import { nativeBackendScopes, nativeBackendLabel } from '../../../packages/ui-client/src/native-backends';
import { nativeOwnerKey, nativeRouteOwner, nativeRouteQuery } from '../../../packages/ui-client/src/native-route';
import type { NativeRouteOwner } from '../../../packages/ui-client/src/native-route';
import { currentNativeInventory, nativeInventoryOrigins } from '../../../packages/ui-client/src/native-inventory';

const hash = useHashRoute();
const mobile = useMediaQuery('(max-width: 767px)');
const section = computed(() => hash.value.slice(2).split("?")[0] || "home");
const query = computed(
  () => new URLSearchParams(hash.value.split("?")[1] ?? ""),
);
const node = computed(() => query.value.get("node") ?? "");
const parsedOwner = computed(() => {
  try { return { owner: nativeRouteOwner(query.value), error: '' }; }
  catch (cause) { return { owner: null, error: cause instanceof Error ? cause.message : String(cause) }; }
});
const selectedOwner = computed(() => parsedOwner.value.owner);
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

const nodes = useRemote(async signal => ({ items: await nativeServiceNodes(client, signal) }), 30000, ['services/agent-manager']);
const hosts = computed(() => nativeHosts(nodes.value.value?.items ?? []));
watch(hosts, values => {
  const previous = values.find(host => host.nodes.some(node => node.serviceNodeId === hostFilter.value));
  if (previous && !values.some(host => host.hostId === hostFilter.value)) filterHost(previous.hostId);
});
const accounts = useRemote(async signal => (await Promise.all((nodes.value.value?.items ?? []).filter(node => node.connected).map(async node => {
  try { return await nativeBackendScopes(client, node, signal); }
  catch { signal.throwIfAborted(); return []; }
}))).flat(), 30000, ['services/agent-manager']);
watch(() => nodes.value.value, () => void accounts.refresh());
const inventoryOrigins = useRemote(signal => nativeInventoryOrigins(client, nodes.value.value?.items ?? [], accounts.value.value ?? [], signal), 0);
watch([() => nodes.value.value, () => accounts.value.value], () => void inventoryOrigins.refresh());
const ownerFor = (ref: Operation.InventoryItem['resourceRef']): NativeRouteOwner => {
  const scope = accounts.value.value?.find(value => value.serviceNodeId === ref.serviceNodeId && (value.backend?.backendId ?? 'codex') === ref.namespace);
  return { serviceNodeId: ref.serviceNodeId, namespace: ref.namespace, ...(scope?.backend ? { backend: scope.backend } : {}) };
};
const accountAvailable = (ref: Pick<Operation.InventoryItem['resourceRef'], 'serviceNodeId' | 'namespace'>) => accounts.value.value?.some(scope =>
  scope.serviceNodeId === ref.serviceNodeId && (scope.backend?.backendId ?? 'codex') === ref.namespace && scope.status.state === 'ready') === true;
const itemKey = (item: Operation.InventoryItem) => taskKey(item.resourceRef.serviceNodeId, item.resourceRef.nativeId, item.resourceRef.namespace, ownerFor(item.resourceRef).backend);
// Read only the visible page. Healthy live subscriptions replace periodic full snapshots.
const recent = usePage(async (signal, cursor) => {
    const page = await client.request(
      "inventory.list",
      {
        ...(hostFilter.value
          ? { hostId: hostFilter.value }
          : { serviceName: "agent-manager" }),
        kind: "thread",
        archived: false,
        sort: "recency-desc",
        ...(search.value ? { searchTerm: search.value } : {}),
        limit: 100,
        ...(cursor ? { cursor } : {}),
      },
      { signal },
    );
    return { items: page.items.filter(isVisibleTask), nextCursor: page.nextCursor };
}, 30000, undefined, () => (accounts.value.value ?? []).map(scope => 'inventory/' + (scope.backend?.backendId ?? 'codex') + '/thread'));
const allProjects = useRemote(async (signal) => {
  const result: ScopedProject[] = [];
  const cursors = new Set<string>();
  let cursor: string | undefined;
  do {
    const page = await client.request("inventory.list", { serviceName: "agent-manager", kind: "project", limit: 200,
      ...(cursor ? { cursor } : {}) }, { signal });
    for (const item of page.items) {
      const project = record(item.summary);
      if (project.source === "native" && typeof project.nativeId === "string" && typeof project.name === "string" && Array.isArray(project.paths))
        result.push({ serviceNodeId: item.resourceRef.serviceNodeId, namespace: item.resourceRef.namespace, project: project as Agent.ProjectSummary });
    }
    cursor = page.nextCursor ?? undefined;
    if (result.length > 1000 || cursor && cursors.has(cursor) || cursors.size >= 20) throw new Error('Project navigation exceeded its bounded snapshot.');
    if (cursor) cursors.add(cursor);
  } while (cursor);
  return result;
}, 30000, () => (accounts.value.value ?? []).map(scope => 'inventory/' + (scope.backend?.backendId ?? 'codex') + '/project'));

const lastNode = ref("");
try {
  lastNode.value =
    sessionStorage.getItem("ivy.agent.last-node:" + base.href) ?? "";
} catch {
  /* Optional view state. */
}
watch(hostFilter, () => {
  recent.value.value = null;
  recent.reset();
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
    if (current === "home" && value && !mobile.value)
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
const newTaskQuery = computed(() => {
  const chosen = hosts.value.find(host => host.hostId === hostFilter.value)?.serviceNodeId || node.value || fallbackNode.value;
  return selectedOwner.value?.serviceNodeId === chosen ? nativeRouteQuery(selectedOwner.value) : { node: chosen };
});
const hostName = (serviceNodeId: string) =>
  hosts.value.find(host => host.hostId === serviceNodeId || host.nodes.some(node => node.serviceNodeId === serviceNodeId))?.hostId ?? serviceNodeLabel(nodes.value.value?.items ?? [], serviceNodeId);
const taskName = (summary: unknown, id: string) =>
  text(record(summary).name) ||
  text(record(summary).preview).slice(0, 100) ||
  id;
const hostItem = (serviceNodeId: string) =>
  hosts.value.find(host => host.hostId === serviceNodeId || host.nodes.some(node => node.serviceNodeId === serviceNodeId));
const hostReady = (serviceNodeId: string) => {
  const item = hostItem(serviceNodeId);
  return !!item?.ready && !!item.connected;
};
const scopedProjects = computed(() =>
  currentNativeInventory(allProjects.value.value ?? [], value => ({ serviceNodeId: value.serviceNodeId, namespace: value.namespace ?? 'codex', kind: 'project', nativeId: value.project.nativeId }),
    inventoryOrigins.value.value ?? [], accounts.value.value ?? [], nodes.value.value?.items ?? []).filter(value => !hostFilter.value || hostName(value.serviceNodeId) === hostFilter.value).map(value => ({ ...value,
    backend: ownerFor({ serviceNodeId: value.serviceNodeId, namespace: value.namespace ?? 'codex', kind: 'project', nativeId: value.project.nativeId }).backend })));
const projectOf = (owner: NativeRouteOwner, id: string) =>
  scopedProjects.value.find(value => nativeOwnerKey({ serviceNodeId: value.serviceNodeId, namespace: value.namespace ?? 'codex', backend: value.backend }) === nativeOwnerKey(owner) && value.project.nativeId === id)?.project;
// Tasks wait for the project catalog so internal work never flashes into a user project.
const tasks = computed(() => allProjects.value.value === null ? [] : currentNativeInventory(recent.value.value?.items ?? [], value => value.resourceRef,
  inventoryOrigins.value.value ?? [], accounts.value.value ?? [], nodes.value.value?.items ?? []));
const taskLists = computed(() => partitionInternalTasks(scopedProjects.value, tasks.value));
const visibleRecent = computed(() => taskLists.value.recent);
const sections = computed(() =>
  hostSections(
    hosts.value.filter(host => !hostFilter.value || host.hostId === hostFilter.value),
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
const trackedTasks = computed(() => new Map(visibleRecent.value.map(item => [
  itemKey(item), item,
])));
// Remove old internal badge state too, including tasks moved into IvyInternal.
watch(() => taskLists.value.internal, (items) => {
  const tasks = { ...seen.value.tasks }, working = new Map(live.value);
  let changed = false;
  for (const item of items) {
    const key = itemKey(item);
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
  section.value === "task" && selectedOwner.value && node.value && query.value.get("id")
    ? taskKey(node.value, query.value.get("id")!, selectedOwner.value.namespace, selectedOwner.value.backend)
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
  taskActivity(item, live.value, seen.value, openKey.value, clock.value, ownerFor(item.resourceRef).backend);
// A live lifecycle event counts as an update before the inventory reports it.
const updatedAt = (item: Operation.InventoryItem) => Math.max(
  (Number(record(item.summary).updatedAt) || 0) * 1000,
  live.value.get(itemKey(item))?.at ?? 0,
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
    const backend = payload.backend as Agent.BackendIdentity | undefined;
    const key = taskKey(value.params.serviceNodeId, threadId, backend?.backendId ?? 'codex', backend);
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
    :settings-href="node && selectedOwner ? route('host-settings', nativeRouteQuery(selectedOwner)) : route('settings')"
    :base="base"
    :client="client"
    :navigation="[]"
    show-usage
    :usage-node="section === 'task' || selectedOwner?.backend ? node : undefined"
    :usage-backend="selectedOwner?.backend"
  >
    <template #sidebar-actions>
      <SidebarGroup class="pb-0">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton v-if="fallbackNode" as-child
              ><a :href="route('host', newTaskQuery)"
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
                      v-for="item in hosts"
                      :key="item.hostId"
                      :value="item.hostId"
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
                :key="itemKey(item)"
                :item="item"
                :available="accountAvailable(item.resourceRef)"
                :backend="ownerFor(item.resourceRef).backend"
                :name="taskName(item.summary, item.resourceRef.nativeId)"
                :active="openKey === itemKey(item)"
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
            :key="host.hostId"
            as-child
            :open="!collapsed.has('host:' + host.hostId)"
            @update:open="toggleGroup('host:' + host.hostId)"
          >
            <SidebarGroup>
              <SidebarGroupLabel class="gap-1.5 pr-8">
                <HostUsage
                  :client="client"
                  passive
                  :node="host.serviceNodeId"
                  :accounts="(accounts.value.value ?? []).filter(scope => scope.hostId === host.hostId)"
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
                            ><span v-if="group.owner.backend" class="truncate text-xs text-muted-foreground">{{ nativeBackendLabel(group.owner.backend) }}</span></SidebarMenuButton
                          >
                        </CollapsibleTrigger>
                        <SidebarMenuAction v-if="group.path" as-child show-on-hover class="right-7"
                          ><a
                            :href="
                              route('host', nativeRouteQuery(group.owner, {
                                project: group.path,
                                projectId: group.projectId,
                              }))
                            "
                            :aria-label="'New task in ' + group.name"
                            ><Plus aria-hidden="true" /></a
                        ></SidebarMenuAction>
                        <ProjectActions v-if="group.path && accountAvailable(group.owner) && (group.owner.backend || group.owner.namespace === 'codex') && projectOf(group.owner, group.projectId)" :client="client" :node="group.owner.serviceNodeId" :backend="group.owner.backend"
                          :project="projectOf(group.owner, group.projectId)!"
                          @changed="allProjects.refresh(); recent.refresh()">
                          <SidebarMenuAction :aria-label="'Manage project ' + group.name" show-on-hover><MoreHorizontal aria-hidden="true" /></SidebarMenuAction>
                        </ProjectActions>
                        <CollapsibleContent>
                          <SidebarMenuSub>
                            <TaskRow
                              v-for="item in group.tasks"
                              :key="itemKey(item)"
                              nested
                              :item="item"
                              :available="accountAvailable(item.resourceRef)"
                              :backend="ownerFor(item.resourceRef).backend"
                              :name="taskName(item.summary, item.resourceRef.nativeId)"
                              :active="openKey === itemKey(item)"
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
                              :key="itemKey(item)"
                              nested
                              :item="item"
                              :available="accountAvailable(item.resourceRef)"
                              :backend="ownerFor(item.resourceRef).backend"
                              :name="taskName(item.summary, item.resourceRef.nativeId)"
                              :active="openKey === itemKey(item)"
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
        <div v-if="recent.page.value > 1 || recent.value.value?.nextCursor" class="flex items-center justify-between gap-2 px-3 py-2">
          <Button variant="ghost" size="sm" :disabled="recent.page.value <= 1 || recent.loading.value" @click="recent.previous">Previous tasks</Button>
          <span class="text-xs text-muted-foreground">{{ recent.page.value }}</span>
          <Button variant="ghost" size="sm" :disabled="!recent.value.value?.nextCursor || recent.loading.value" @click="recent.next">Next tasks</Button>
        </div>
      </nav>
    </template>
    <RemoteState v-if="parsedOwner.error" :loading="false" :error="parsedOwner.error" :has-data="false" />
    <AgentTask
      v-else-if="section === 'task' && node && query.get('id') && selectedOwner"
      :key="nativeOwnerKey(selectedOwner) + ':' + query.get('id')"
      :node="node"
      :backend="selectedOwner.backend"
      :thread-id="query.get('id')!"
      :turn-id="query.get('turn') ?? ''"
    />
    <AgentHostSettings
      v-else-if="section === 'host-settings' && node"
      :key="selectedOwner ? nativeOwnerKey(selectedOwner) : node"
      :node="node"
      :backend="selectedOwner?.backend"
    />
    <AgentSettings v-else-if="section === 'settings'" :node="managementNode" />
    <AgentHostAccounts
      v-else-if="section === 'host' && node"
      :key="hash"
      :node="node"
      :nodes="nodes.value.value?.items ?? []"
      :backend="selectedOwner?.backend"
      :project="query.get('project') ?? ''"
      :project-id="query.get('projectId') ?? ''"
    />
    <AgentHostAccounts
      v-else-if="section === 'home' && fallbackNode && !mobile"
      :key="fallbackNode"
      :node="fallbackNode"
      :nodes="nodes.value.value?.items ?? []"
      project=""
      project-id=""
    />
    <AgentHosts
      v-else
      :nodes="nodes.value.value?.items ?? []"
      :accounts="accounts.value.value ?? []"
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
      :key="itemKey(item)"
      :href="
        route('task', nativeRouteQuery(ownerFor(item.resourceRef), {
          id: item.resourceRef.nativeId,
        }))
      "
      :title="taskName(item.summary, item.resourceRef.nativeId)"
      :detail="hostName(item.resourceRef.serviceNodeId) + (ownerFor(item.resourceRef).backend ? ' · ' + nativeBackendLabel(ownerFor(item.resourceRef).backend!) : '')"
    />
  </SearchDialog>
</template>

<style scoped>
.agent-sidebar {
  min-width: 0;
  overflow-x: hidden;
}
</style>
