<script setup lang="ts">
import { computed, ref } from 'vue';
import { useElementSize } from '@vueuse/core';
import { useRemote } from '@ivy/ui';
import type { HiveClient, Operation } from '../../sdk/src/client.js';
import HostUsage from './HostUsage.vue';
import { serviceNodeLabel } from './native';

const props = defineProps<{ client: HiveClient; host?: string | null | undefined; node?: string | undefined }>();
const remote = useRemote(async signal => {
  const items: Operation.ServiceNode[] = [];
  let cursor: string | null = null;
  do {
    const page: Operation.ServiceNodesListResult = await props.client.request('serviceNodes.list', {
      serviceName: 'agent-manager', limit: 200, ...(cursor ? { cursor } : {}),
    }, { signal });
    items.push(...page.items); cursor = page.nextCursor;
  } while (cursor);
  return items;
}, 30000, ['services/agent-manager']);
const nodes = computed(() => (remote.value.value ?? []).filter(item =>
  props.node ? item.serviceNodeId === props.node : props.host === undefined || item.hostId === props.host));
const element = ref<HTMLElement>();
const { width } = useElementSize(element);
const compact = computed(() => width.value < nodes.value.length * 112);
</script>

<template>
  <div v-if="nodes.length" ref="element" data-slot="host-usage" class="host-usage-pills" :style="{ '--usage-count': nodes.length }" aria-label="Host usage">
    <HostUsage v-for="item in nodes" :key="item.serviceNodeId" :client="client" :node="item.serviceNodeId"
      :label="serviceNodeLabel(remote.value.value ?? [], item.serviceNodeId)" :ready="item.ready && item.connected" :compact="compact" />
  </div>
</template>

<style scoped>
.host-usage-pills { display: flex; flex: 0 1 auto; align-items: center; justify-content: flex-end; gap: .25rem; width: calc(var(--usage-count) * 8.5rem); min-width: calc(var(--usage-count) * 1.75rem + (var(--usage-count) - 1) * .25rem); max-width: 40vw; }
@media (max-width: 767px) { .host-usage-pills { width: calc(var(--usage-count) * 1.75rem + (var(--usage-count) - 1) * .25rem); max-width: none; } }
</style>
