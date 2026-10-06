<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { PageHeader, RemoteState, useRemote } from "@ivy/ui";
import UiFrame from "../../../packages/ui-client/src/UiFrame.vue";
import { useHashRoute } from "../../../packages/ui-client/src/runtime";
import { base, client } from "./runtime";
import type { Operation } from "./runtime";
import Workspace from "./Workspace.vue";

const hash = useHashRoute();
const query = computed(
  () => new URLSearchParams(hash.value.split("?")[1] ?? ""),
);
const requestedNode = computed(() => query.value.get("node") ?? "");
const section = computed(() => hash.value.slice(2).split("?")[0] || "home");
const services = useRemote(async (signal) => {
  const items: Operation.ServiceNode[] = [];
  const seen = new Set<string>();
  let cursor: string | null = null;
  do {
    const page: Operation.ServiceNodesListResult = await client.request(
      "serviceNodes.list",
      { serviceName: "task-board", limit: 200, ...(cursor ? { cursor } : {}) },
      { signal },
    );
    items.push(...page.items);
    cursor = page.nextCursor;
    if (cursor && seen.has(cursor))
      throw new Error("TaskBoard discovery could not finish.");
    if (cursor) seen.add(cursor);
  } while (cursor);
  return items;
}, 30000, ["services"]);

const lastNode = ref("");
try {
  lastNode.value =
    sessionStorage.getItem("ivy.task-board.last-node:" + base.href) ?? "";
} catch {
  /* Optional view state. */
}
const node = computed(() => {
  const available = services.value.value ?? [];
  return (
    available.find((item) => item.serviceNodeId === requestedNode.value)
      ?.serviceNodeId ??
    available.find((item) => item.serviceNodeId === lastNode.value)
      ?.serviceNodeId ??
    available.find((item) => item.ready && item.connected)?.serviceNodeId ??
    available[0]?.serviceNodeId ??
    ""
  );
});
watch(node, (value) => {
  if (!value) return;
  lastNode.value = value;
  try {
    sessionStorage.setItem("ivy.task-board.last-node:" + base.href, value);
  } catch {
    /* Optional view state. */
  }
});
</script>

<template>
  <Workspace
    v-if="node"
    :key="node"
    :node="node"
    :services="services.value.value ?? []"
    :section="section"
    :query="query"
  />
  <UiFrame
    v-else
    ui-name="TaskBoard"
    :base="base"
    :client="client"
    :navigation="[]"
  >
    <PageHeader
      title="TaskBoard"
      description="Plan, run and review work."
      :loading="services.loading.value"
      :updated-at="services.updatedAt.value"
    />
    <RemoteState
      :loading="services.loading.value"
      :error="services.error.value"
      :has-data="!!services.value.value"
      :empty="services.value.value?.length === 0"
      empty-title="No TaskBoard workspace is connected"
      empty-detail="Start a configured TaskBoard service to plan and review work here."
      @retry="services.refresh"
    />
  </UiFrame>
</template>
