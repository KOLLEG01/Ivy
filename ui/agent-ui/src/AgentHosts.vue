<script setup lang="ts">
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
  Item,
  ItemActions,
  ItemContent,
  ItemDescription,
  ItemGroup,
  ItemMedia,
  ItemTitle,
  PageHeader,
  RemoteState,
  StatusBadge,
} from "@ivy/ui";
import { ArrowRight, Server } from "@lucide/vue";
import type { Operation } from "../../../packages/sdk/src/client.js";
import { dateLabel, route } from "../../../packages/ui-client/src/runtime";
import { record, serviceNodeLabel, text } from "../../../packages/ui-client/src/native";

type AgentNode = {
  serviceNodeId: string;
  hostId: string;
  ready: boolean;
  connected: boolean;
};
const props = defineProps<{
  nodes: AgentNode[];
  tasks: Operation.InventoryItem[];
  loading: boolean;
  error: string | null;
}>();
defineEmits<{ refresh: [] }>();

const hostName = (serviceNodeId: string) =>
  serviceNodeLabel(props.nodes, serviceNodeId);
const taskName = (summary: unknown, id: string) =>
  text(record(summary).name) ||
  text(record(summary).preview).slice(0, 140) ||
  id;
</script>

<template>
  <div class="agent-overview mx-auto w-full max-w-5xl">
    <PageHeader
      title="All hosts"
      description="Browse tasks and hosts in this workspace."
      :loading="loading"
    />
    <RemoteState
      :loading="loading"
      :error="error"
      :has-data="!!nodes.length || !!tasks.length"
      @retry="$emit('refresh')"
    />

    <section aria-labelledby="hosts-heading" class="mb-10">
      <div class="mb-3 flex items-center justify-between gap-3">
        <h2 id="hosts-heading" class="text-sm font-medium">Hosts</h2>
        <span class="text-xs text-muted-foreground"
          >{{ nodes.filter((item) => item.ready && item.connected).length }} of
          {{ nodes.length }} ready</span
        >
      </div>
      <ItemGroup class="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
        <Item
          v-for="item in nodes"
          :key="item.serviceNodeId"
          as-child
          variant="outline"
          class="rounded-xl"
        >
          <a :href="route('host', { node: item.serviceNodeId })">
            <ItemMedia variant="icon" class="size-8 rounded-md bg-muted"
              ><Server aria-hidden="true"
            /></ItemMedia>
            <ItemContent
              ><ItemTitle>{{ hostName(item.serviceNodeId) }}</ItemTitle
              ><ItemDescription>{{
                item.ready && item.connected
                  ? "Ready"
                  : item.connected
                    ? "Starting"
                    : "Offline"
              }}</ItemDescription></ItemContent
            >
            <ItemActions
              ><StatusBadge
                v-if="!item.ready || !item.connected"
                :label="item.connected ? 'Not ready' : 'Offline'"
                tone="warning" /><ArrowRight
                v-else
                class="size-4 text-muted-foreground"
                aria-hidden="true"
            /></ItemActions>
          </a>
        </Item>
      </ItemGroup>
      <Empty v-if="!nodes.length && !loading" class="border"
        ><EmptyMedia variant="icon"><Server aria-hidden="true" /></EmptyMedia
        ><EmptyHeader
          ><EmptyTitle>No AgentManager</EmptyTitle
          ><EmptyDescription
            >Connect an AgentManager to run tasks on this
            workspace.</EmptyDescription
          ></EmptyHeader
        ></Empty
      >
    </section>

    <section aria-labelledby="tasks-heading">
      <div class="mb-3 flex items-center justify-between gap-3">
        <h2 id="tasks-heading" class="text-sm font-medium">Recent tasks</h2>
      </div>
      <ItemGroup>
        <Item
          v-for="item in tasks"
          :key="
            item.resourceRef.serviceNodeId + ':' + item.resourceRef.nativeId
          "
          as-child
          variant="outline"
          class="rounded-xl"
        >
          <a
            :href="
              route('task', {
                node: item.resourceRef.serviceNodeId,
                id: item.resourceRef.nativeId,
              })
            "
            :aria-label="taskName(item.summary, item.resourceRef.nativeId)"
            ><ItemMedia
              ><span
                class="size-2 rounded-full bg-emerald-500"
                aria-hidden="true" /></ItemMedia
            ><ItemContent
              ><ItemTitle>{{
                taskName(item.summary, item.resourceRef.nativeId)
              }}</ItemTitle
              ><ItemDescription
                >{{ hostName(item.resourceRef.serviceNodeId) }} ·
                {{ text(record(item.summary).cwd) }} ·
                {{ dateLabel(item.observedAt) }}</ItemDescription
              ></ItemContent
            ><ItemActions
              ><ArrowRight
                class="size-4 text-muted-foreground"
                aria-hidden="true" /></ItemActions
          ></a>
        </Item>
      </ItemGroup>
      <Empty v-if="!tasks.length && !loading" class="border"
        ><EmptyHeader
          ><EmptyTitle>No active tasks</EmptyTitle
          ><EmptyDescription
            >Connected hosts have no recent tasks.</EmptyDescription
          ></EmptyHeader
        ></Empty
      >
    </section>
  </div>
</template>

<style scoped>
.agent-overview {
  padding-block: clamp(1rem, 4vh, 3rem);
}
.agent-overview :deep(header) {
  border-bottom: 0;
}
</style>
