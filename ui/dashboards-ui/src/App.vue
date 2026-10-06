<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { Presentation, Plus } from "@lucide/vue";
import {
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
  SidebarMenuButton,
  SidebarMenuItem,
} from "@ivy/ui";
import UiFrame from "../../../packages/ui-client/src/UiFrame.vue";
import { route, useHashRoute } from "../../../packages/ui-client/src/runtime";
import { base, client, call, tr } from "./runtime";
import type { Dashboard, Row } from "./runtime";
import DashboardEditor from "./DashboardEditor.vue";
import DashboardView from "./DashboardView.vue";
import Viewer from "./Viewer.vue";

const hash = useHashRoute();
const section = computed(() => hash.value.slice(2).split("?")[0] || "home");
const query = computed(
  () => new URLSearchParams(hash.value.split("?")[1] ?? ""),
);
// `#/view` is the frameless display that saved dashboards link to; it has no shell.
const viewing = computed(() => section.value === "view");
const dashboardId = computed(() => query.value.get("id") ?? "");
const node = ref(""),
  nodes = ref<{ serviceNodeId: string; ready: boolean }[]>([]);
const rows = ref<
    { id: string; revision: number; title: string; url: string }[]
  >([]),
  nextCursor = ref<string | null>(null),
  loaded = ref(false);
const busy = ref(false),
  error = ref(""),
  selected = ref<Row | null>(null);
const go = (target: string) => {
  window.location.hash = target;
};
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
async function list(append = false) {
  if (!node.value) return;
  const page = await call<{
    items: typeof rows.value;
    nextCursor: string | null;
  }>(
    node.value,
    "list",
    append && nextCursor.value ? { cursor: nextCursor.value } : {},
  );
  rows.value = append ? [...rows.value, ...page.items] : page.items;
  nextCursor.value = page.nextCursor;
  loaded.value = true;
}
async function initialize() {
  await perform(async () => {
    let cursor: string | undefined;
    const found = [];
    do {
      const page = await client.request("serviceNodes.list", {
        serviceName: "dashboards",
        limit: 100,
        ...(cursor ? { cursor } : {}),
      });
      found.push(...page.items);
      cursor = page.nextCursor ?? undefined;
    } while (cursor);
    nodes.value = found;
    node.value =
      found.find((n) => n.ready)?.serviceNodeId ??
      found[0]?.serviceNodeId ??
      "";
    await list();
  });
}
watch(
  viewing,
  (value, old) => {
    if (!value && old !== false) void initialize();
  },
  { immediate: true },
);
// View and edit always read the stored dashboard and its revision.
watch(
  [section, dashboardId, node],
  ([name, id]) => {
    if (!["dashboard", "edit"].includes(name) || !id || !node.value) {
      selected.value = null;
      return;
    }
    if (selected.value?.id === id) return;
    void perform(async () => {
      selected.value = await call<Row>(node.value, "read", { id });
    });
  },
  { immediate: true },
);
const save = (value: Dashboard) =>
  perform(async () => {
    const saved =
      selected.value && section.value === "edit" ? selected.value : null;
    const result = await call<Omit<Row, "value">>(
      node.value,
      "save",
      {
        ...(saved ? { id: saved.id, expectedRevision: saved.revision } : {}),
        value,
      },
      true,
    );
    selected.value = { ...result, value };
    await list();
    if (!saved) go(route("edit", { id: result.id }));
  });
const remove = () =>
  perform(async () => {
    const row = selected.value!;
    await call(
      node.value,
      "delete",
      { id: row.id, expectedRevision: row.revision },
      true,
    );
    selected.value = null;
    await list();
    go(route("home"));
  });
const changeNode = () => {
  rows.value = [];
  loaded.value = false;
  go(route("home"));
  void perform(() => list());
};
</script>

<template>
  <main v-if="viewing" class="h-dvh w-screen overflow-hidden">
    <Viewer
      v-if="query.get('node') && dashboardId"
      :key="query.get('node')! + dashboardId"
      :node="query.get('node')!"
      :id="dashboardId"
      :color-mode="query.get('colorMode') ?? 'color'"
    />
    <p v-else role="alert">
      {{ tr("Dashboard-URL unvollständig.", "Incomplete dashboard URL.") }}
    </p>
  </main>
  <UiFrame
    v-else
    ui-name="Dashboards"
    :base="base"
    :client="client"
    :navigation="[]"
  >
    <template #sidebar-actions>
      <SidebarGroup class="pb-0">
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton as-child :is-active="section === 'new'">
              <a :href="route('new')"
                ><Plus aria-hidden="true" /><span>{{
                  tr("Neues Dashboard", "New dashboard")
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
            id="dashboards-node"
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
        <SidebarGroupLabel>Dashboards</SidebarGroupLabel>
        <SidebarGroupContent>
          <SidebarMenu>
            <SidebarMenuItem v-for="row in rows" :key="row.id">
              <SidebarMenuButton as-child :is-active="dashboardId === row.id">
                <a
                  :href="route('dashboard', { id: row.id })"
                  :aria-current="dashboardId === row.id ? 'page' : undefined"
                  ><Presentation aria-hidden="true" /><span>{{
                    row.title
                  }}</span></a
                >
              </SidebarMenuButton>
            </SidebarMenuItem>
          </SidebarMenu>
          <Button
            v-if="nextCursor"
            variant="ghost"
            size="sm"
            class="mx-2"
            :disabled="busy"
            @click="perform(() => list(true))"
            >{{ tr("Mehr laden", "Load more") }}</Button
          >
        </SidebarGroupContent>
      </SidebarGroup>
    </template>

    <Alert v-if="error" variant="destructive" class="mb-6"
      ><AlertDescription>{{ error }}</AlertDescription></Alert
    >

    <template v-if="section === 'new' || (section === 'edit' && selected)">
      <PageHeader
        :title="
          section === 'new'
            ? tr('Neues Dashboard', 'New dashboard')
            : selected!.value.title
        "
      />
      <DashboardEditor
        :key="section + (selected?.id ?? '')"
        :node="node"
        :row="section === 'new' ? null : selected"
        :busy="busy || !node"
        @save="save"
        @cancel="
          go(
            section === 'new'
              ? route('home')
              : route('dashboard', { id: dashboardId }),
          )
        "
      />
    </template>
    <template v-else-if="section === 'dashboard' && selected">
      <PageHeader :title="selected.value.title" />
      <DashboardView
        :node="node"
        :row="selected"
        :busy="busy"
        @edit="go(route('edit', { id: selected.id }))"
        @remove="remove"
      />
    </template>
    <template v-else-if="section === 'home' || !dashboardId">
      <PageHeader
        title="Dashboards"
        :description="
          tr(
            'HTML-Dashboards mit aktuellen Hive-Daten.',
            'HTML dashboards with live Hive data.',
          )
        "
        :loading="busy"
      />
      <Empty v-if="!busy && !node" class="border border-dashed">
        <EmptyHeader>
          <EmptyMedia variant="icon"
            ><Presentation aria-hidden="true"
          /></EmptyMedia>
          <EmptyTitle>{{
            tr("Kein Dashboards-Dienst", "No Dashboards service")
          }}</EmptyTitle>
          <EmptyDescription>{{
            tr(
              "Kein Dashboards-Dienst verbunden.",
              "No Dashboards service is connected.",
            )
          }}</EmptyDescription>
        </EmptyHeader>
      </Empty>
      <Empty v-else-if="loaded && !rows.length" class="border border-dashed">
        <EmptyHeader>
          <EmptyMedia variant="icon"
            ><Presentation aria-hidden="true"
          /></EmptyMedia>
          <EmptyTitle>{{
            tr("Noch keine Dashboards", "No dashboards yet")
          }}</EmptyTitle>
          <EmptyDescription>{{
            tr(
              "Ein Dashboard zeigt Hive-Daten in eigenem HTML und aktualisiert sich selbst.",
              "A dashboard shows Hive data in your own HTML and keeps itself up to date.",
            )
          }}</EmptyDescription>
        </EmptyHeader>
        <EmptyContent
          ><Button as-child
            ><a :href="route('new')"
              ><Plus aria-hidden="true" />{{
                tr("Neues Dashboard", "New dashboard")
              }}</a
            ></Button
          ></EmptyContent
        >
      </Empty>
      <div v-else class="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
        <a
          v-for="row in rows"
          :key="row.id"
          :href="route('dashboard', { id: row.id })"
          class="group rounded-xl outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
        >
          <Card
            class="h-full gap-3 transition-colors group-hover:border-foreground/20 group-hover:bg-accent/50"
          >
            <CardHeader>
              <span
                class="mb-2 flex size-9 items-center justify-center rounded-md bg-secondary text-primary"
                ><Presentation class="size-4" aria-hidden="true"
              /></span>
              <CardTitle>{{ row.title }}</CardTitle>
              <CardDescription
                >{{ tr("Revision", "Revision") }}
                {{ row.revision }}</CardDescription
              >
            </CardHeader>
          </Card>
        </a>
      </div>
    </template>
  </UiFrame>
</template>
