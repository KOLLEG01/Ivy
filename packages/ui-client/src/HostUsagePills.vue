<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useElementSize } from '@vueuse/core';
import { useRemote } from '@ivy/ui';
import type { Agent, HiveClient } from '../../sdk/src/client.js';
import HostUsage from './HostUsage.vue';
import { nativeHosts, nativeServiceNodes } from './native-hosts';
import { nativeBackendScopes } from './native-backends';

const props = defineProps<{ client: HiveClient; host?: string | null | undefined; node?: string | undefined; backend?: Agent.BackendIdentity | undefined }>();
const remote = useRemote(signal => nativeServiceNodes(props.client, signal), 30000, ['services/agent-manager']);
const nodes = computed(() => (remote.value.value ?? []).filter(item =>
  props.node ? item.serviceNodeId === props.node : props.host === undefined || item.hostId === props.host));
const hosts = computed(() => nativeHosts(nodes.value));
const accounts = useRemote(async signal => (await Promise.all(nodes.value.filter(node => node.connected).map(async node => {
  try { return await nativeBackendScopes(props.client, node, signal); }
  catch { signal.throwIfAborted(); return []; }
}))).flat(), 30000, ['services/agent-manager']);
watch(nodes, () => void accounts.refresh());
const element = ref<HTMLElement>();
const { width } = useElementSize(element);
// Captions need about 19rem per host and unlabelled bars about 6rem; otherwise one ring.
const level = computed(() => width.value >= hosts.value.length * 300 ? 'full' : width.value >= hosts.value.length * 96 ? 'meters' : 'mark');
</script>

<template>
  <div v-if="hosts.length" ref="element" data-slot="host-usage" class="host-usage-pills" :style="{ '--usage-count': hosts.length }" aria-label="Host usage">
    <HostUsage v-for="item in hosts" :key="item.hostId" :client="client" :node="item.serviceNodeId" :backend="backend"
      :accounts="(accounts.value.value ?? []).filter(scope => scope.hostId === item.hostId)"
      :label="item.hostId" :ready="item.ready && item.connected" :level="level" />
  </div>
</template>

<style scoped>
.host-usage-pills { display: flex; flex: 0 1 auto; align-items: center; justify-content: flex-end; gap: .25rem; width: calc(var(--usage-count) * 19rem); min-width: calc(var(--usage-count) * 1.75rem + (var(--usage-count) - 1) * .25rem); max-width: 55vw; }
@media (max-width: 767px) { .host-usage-pills { width: max(calc(var(--usage-count) * 1.75rem + (var(--usage-count) - 1) * .25rem), min(6rem, 30vw)); max-width: none; } }
</style>
