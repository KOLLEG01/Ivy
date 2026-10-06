<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { RemoteState, useRemote } from '@ivy/ui';
import type { Agent, BrowserNotifications, RpcClient } from '../../sdk/src/client.js';
import { nativeInputUpdates } from './live';
import { nativeRead } from './native';
import NativeInput from './NativeInput.vue';
const props = defineProps<{ hideEmpty?: boolean; retainDrafts?: boolean; node: string; threadId: string; turnId?: string; ownerEpoch?: string | undefined; base: URL; client: RpcClient; notifications: BrowserNotifications }>();
const emit = defineEmits<{ attention: [count: number] }>();
const { client } = props;
const pending = useRemote(async signal => await nativeRead(client, props.node, 'agent.inputs', { includeExpired: true, limit: 128 }, signal) as Agent.PendingInputPage, 15000,
  nativeInputUpdates(props.notifications, props.node, props.threadId));
const observed = computed(() => pending.value.value?.items.filter(input => input.threadId === props.threadId && (!props.turnId || !input.turnId || input.turnId === props.turnId)) ?? []);
const observedIds = computed(() => new Set(observed.value.map(input => JSON.stringify(input.identity))));
const retained = ref(new Map<string, Agent.PendingInput>());
watch(observed, inputs => {
  if (!props.retainDrafts) return;
  const next = new Map(inputs.map(input => [JSON.stringify(input.identity), input]));
  for (const [id, input] of retained.value) {
    if (next.has(id) || next.size >= 256 || input.threadId !== props.threadId) continue;
    // Keep an already visible response draft when a restarted owner drops its request.
    // It remains bound to the old identity and cannot be submitted against this snapshot.
    const key = 'ivy:agent-input:' + props.base.href + id;
    try { if (sessionStorage.getItem(key + ':draft') || sessionStorage.getItem(key)) next.set(id, input); } catch { next.set(id, input); }
  }
  retained.value = next;
});
const items = computed(() => props.retainDrafts ? [...retained.value.values()] : observed.value);
watch(() => observed.value.filter(item => ['pending', 'answering', 'outcome_unknown'].includes(item.state)).length, count => emit('attention', count));
</script>
<template><section v-if="!hideEmpty || items.length || pending.error.value || pending.value.value?.truncated" class="space-y-4" aria-labelledby="native-input-heading"><div class="flex flex-wrap items-center justify-between gap-3"><h2 id="native-input-heading" class="text-lg font-semibold">Native questions and approvals</h2></div>
  <RemoteState :loading="pending.loading.value" :error="pending.error.value" :has-data="!!pending.value.value" @retry="pending.refresh" /><p v-if="pending.value.value && !items.length" class="text-sm text-muted-foreground">No retained request for this task in the current snapshot.</p>
  <p v-if="pending.value.value?.truncated" class="text-sm text-muted-foreground">The bounded owner snapshot is truncated; current pending inputs are ordered first.</p>
  <p v-if="ownerEpoch && pending.value.value?.epoch !== ownerEpoch" class="text-sm text-muted-foreground">This run belongs to an earlier native connection. Its retained requests are not answerable here.</p>
  <div v-for="input in items" :key="JSON.stringify(input.identity)" class="space-y-2"><p v-if="!observedIds.has(JSON.stringify(input.identity))" class="text-sm text-muted-foreground">This request is no longer in the current snapshot. Your response draft is retained.</p><NativeInput :base="base" :client="client" :input="input" :epoch="pending.value.value?.epoch ?? null" :available="observedIds.has(JSON.stringify(input.identity)) && !pending.error.value && (!ownerEpoch || pending.value.value?.epoch === ownerEpoch)" @answered="pending.refresh" /></div>
</section></template>
