<script setup lang="ts">
import { computed } from "vue";
import {
  Button,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuTrigger,
  RemoteState,
  useRemote,
} from "@ivy/ui";
import { Check, ChevronsUpDown, MonitorDot } from "@lucide/vue";
import UiFrame from "../../../packages/ui-client/src/UiFrame.vue";
import { nativeRead, serviceNodeLabel } from "../../../packages/ui-client/src/native";
import { route } from "../../../packages/ui-client/src/runtime";
import { base, client } from "./runtime";
import type { Operation, TaskBoard } from "./runtime";
import TaskList from "./TaskList.vue";
import TaskDetail from "./TaskDetail.vue";
import TaskEditor from "./TaskEditor.vue";
import SettingsView from "./SettingsView.vue";

const props = defineProps<{
  node: string;
  services: Operation.ServiceNode[];
  section: string;
  query: URLSearchParams;
}>();
const workspace = useRemote(
  async (signal) =>
    (await nativeRead(
      client,
      props.node,
      "task-board.workspace",
      {},
      signal,
    )) as TaskBoard.WorkspaceInfo,
  15000,
  () => ["services/task-board/" + props.node, "objects/task-board"],
);
const identity = computed(() =>
  workspace.value.value
    ? JSON.stringify([
        workspace.value.value.principalId,
        workspace.value.value.rootObjectId,
        workspace.value.value.callerPrincipalId,
      ])
    : "",
);
const switchHref = (node: string) =>
  route(props.section === "settings" ? "settings" : "tasks", { node });
</script>

<template>
  <UiFrame
    ui-name="TaskBoard"
    :base="base"
    :client="client"
    :navigation="[]"
    :settings-href="route('settings', { node })"
  >
    <template v-if="services.length > 1" #context>
      <DropdownMenu>
        <DropdownMenuTrigger as-child>
          <Button
            variant="ghost"
            size="sm"
            class="min-w-0"
            aria-label="Choose TaskBoard workspace"
          >
            <MonitorDot aria-hidden="true" />
            <span class="truncate">{{ serviceNodeLabel(services, node) }}</span>
            <ChevronsUpDown aria-hidden="true" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" class="w-60">
          <DropdownMenuLabel class="text-xs text-muted-foreground"
            >TaskBoard workspace</DropdownMenuLabel
          >
          <DropdownMenuItem
            v-for="item in services"
            :key="item.serviceNodeId"
            as-child
          >
            <a :href="switchHref(item.serviceNodeId)">
              <span
                class="size-2 rounded-full"
                :class="
                  item.ready && item.connected
                    ? 'bg-emerald-500'
                    : 'bg-amber-500'
                "
                aria-hidden="true"
              />
              <span class="min-w-0 flex-1 truncate">{{ serviceNodeLabel(services, item.serviceNodeId) }}</span>
              <Check v-if="item.serviceNodeId === node" aria-hidden="true" />
            </a>
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </template>

    <p
      v-if="workspace.value.value && !workspace.value.value.recovered"
      role="status"
      class="mb-4 text-sm text-muted-foreground"
    >
      Recovering existing work…
    </p>
    <RemoteState
      :loading="workspace.loading.value"
      :error="workspace.error.value"
      :has-data="!!workspace.value.value"
      @retry="workspace.refresh"
    />
    <template v-if="workspace.value.value">
      <SettingsView
        v-if="section === 'settings'"
        :key="identity"
        :workspace="workspace.value.value"
        :available="!workspace.error.value"
      />
      <TaskDetail
        v-else-if="section === 'tasks' && query.get('id') && query.get('panel') !== 'sheet'"
        :key="identity + query.get('id')"
        :workspace="workspace.value.value"
        :available="!workspace.error.value"
        :task-id="query.get('id')!"
        :query="query"
      />
      <TaskEditor
        v-else-if="section === 'new'"
        :key="identity"
        :workspace="workspace.value.value"
        :available="!workspace.error.value"
      />
      <TaskList
        v-else
        :key="identity"
        :workspace="workspace.value.value"
        :available="!workspace.error.value"
        :query="query"
      />
    </template>
  </UiFrame>
</template>
