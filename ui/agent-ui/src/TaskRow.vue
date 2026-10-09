<script setup lang="ts">
import { computed } from "vue";
import { MoreHorizontal } from "@lucide/vue";
import {
  ActivityIndicator,
  HostMark,
  SidebarMenuAction,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSubButton,
  SidebarMenuSubItem,
} from "@ivy/ui";
import type { Operation } from "../../../packages/sdk/src/client.js";
import { record, text } from "../../../packages/ui-client/src/native";
import { route } from "../../../packages/ui-client/src/runtime";
import type { TaskActivity } from "./task-activity";
import TaskActions from "./TaskActions.vue";

// One task in the sidebar: a link, its live state or age, and the task menu on hover or focus.
// `nested` rows sit inside a project; their hover state must not follow the project row.
const props = defineProps<{
  item: Operation.InventoryItem;
  name: string;
  active: boolean;
  activity: TaskActivity;
  nested?: boolean;
  host?: string;
  age?: string;
}>();
const emit = defineEmits<{ changed: [] }>();
const node = computed(() => props.item.resourceRef.serviceNodeId),
  id = computed(() => props.item.resourceRef.nativeId);
const status = computed(() => props.nested
  ? "pointer-events-none absolute top-1.5 right-1 max-md:right-7 md:group-hover/menu-sub-item:opacity-0 md:group-focus-within/menu-sub-item:opacity-0"
  : "pointer-events-none absolute top-2 right-1.5 max-md:right-8 md:group-hover/menu-item:opacity-0 md:group-focus-within/menu-item:opacity-0");
const menu = computed(() => props.nested
  ? "top-1 md:opacity-0 group-hover/menu-sub-item:opacity-100 group-focus-within/menu-sub-item:opacity-100 data-[state=open]:opacity-100"
  : "md:opacity-0 group-hover/menu-item:opacity-100 group-focus-within/menu-item:opacity-100 data-[state=open]:opacity-100");
</script>
<template>
  <component :is="nested ? SidebarMenuSubItem : SidebarMenuItem">
    <component
      :is="nested ? SidebarMenuSubButton : SidebarMenuButton"
      as-child
      :is-active="active"
      :class="activity || age ? 'pr-7 max-md:pr-14' : 'pr-7'"
    >
      <a
        :href="route('task', { node, id })"
        :aria-current="active ? 'page' : undefined"
        :title="host ? host + ' · ' + name : name"
        ><HostMark v-if="host" :label="host" :seed="node" /><span>{{ name }}</span></a
      >
    </component>
    <ActivityIndicator v-if="activity" :state="activity" :class="status" />
    <span v-else-if="age" :class="status" class="h-4 text-xs leading-4 text-muted-foreground tabular-nums">{{ age }}</span>
    <TaskActions
      :node="node"
      :thread-id="id"
      :name="name"
      :project-id="text(record(item.summary).projectId)"
      :archived="record(item.summary).archived === true"
      :working="activity === 'working'"
      scope="sidebar"
      align="start"
      @changed="emit('changed')"
    >
      <SidebarMenuAction :class="menu" :aria-label="'Actions for ' + name"
        ><MoreHorizontal aria-hidden="true"
      /></SidebarMenuAction>
    </TaskActions>
  </component>
</template>
