<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { PageHeader, RemoteState, useRemote } from "@ivy/ui";
import UiFrame from "../../../packages/ui-client/src/UiFrame.vue";
import { useHashRoute } from "../../../packages/ui-client/src/runtime";
import type { Operation } from "../../../packages/sdk/src/client.js";
import { base, client, route, tr } from "./runtime";
import Workspace from "./Workspace.vue";

const hash = useHashRoute();
const section = computed(() => hash.value.slice(2).split("?")[0] || "journal");
const services = useRemote(async (signal) => {
  const items: Operation.ServiceNode[] = [];
  let cursor: string | null = null;
  do {
    const page: Operation.ServiceNodesListResult = await client.request(
      "serviceNodes.list",
      { serviceName: "secretary", limit: 200, ...(cursor ? { cursor } : {}) },
      { signal },
    );
    items.push(...page.items);
    cursor = page.nextCursor;
  } while (cursor);
  return items;
}, 30000, ["services"]);
const selected = ref(new URLSearchParams(location.hash.split("?")[1] ?? "").get("node") ?? "");
const node = computed(() => {
  const values = services.value.value ?? [];
  return (
    values.find((item) => item.serviceNodeId === selected.value)
      ?.serviceNodeId ??
    values.find((item) => item.ready && item.connected)?.serviceNodeId ??
    values[0]?.serviceNodeId ??
    ""
  );
});
watch(node, (value) => {
  if (value) selected.value = value;
});
</script>
<template>
  <Workspace
    v-if="node"
    :key="node"
    :node="node"
    :services="services.value.value ?? []"
    :section="section"
    @select-node="selected = $event"
  />
  <UiFrame
    v-else
    ui-name="Secretary"
    :base="base"
    :client="client"
    :navigation="[]"
  >
    <PageHeader
      title="Secretary"
      :description="
        tr(
          'Systemaufträge konfigurieren und ihre Ausführungen verfolgen.',
          'Configure system assignments and review their executions.',
        )
      "
      :loading="services.loading.value"
    />
    <RemoteState
      :loading="services.loading.value"
      :error="services.error.value"
      :has-data="!!services.value.value"
      :empty="services.value.value?.length === 0"
      :empty-title="tr('Kein Secretary verbunden', 'No Secretary is connected')"
      :empty-detail="
        tr(
          'Starte einen konfigurierten Secretary-Dienst.',
          'Start a configured Secretary service.',
        )
      "
      @retry="services.refresh"
    />
    <a class="sr-only" :href="route('journal')">Journal</a>
  </UiFrame>
</template>
