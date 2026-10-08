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
const completed = ref(new Set<string>());
const responseKey = (id: string) => 'ivy:agent-input:' + props.base.href + id;
const responsePhase = (id: string) => {
  try { return (JSON.parse(sessionStorage.getItem(responseKey(id)) ?? 'null') as { phase?: string } | null)?.phase; }
  catch { return 'unknown'; }
};
// This tab still owes a response: an unsent draft, or an answer whose outcome is not confirmed yet.
const unresolved = (id: string) => {
  try {
    const phase = responsePhase(id);
    return phase ? phase !== 'succeeded' : !!sessionStorage.getItem(responseKey(id) + ':draft');
  } catch { return true; }
};
const resolved = (input: Agent.PendingInput) => input.state === 'answered' ||
  input.state === 'expired' && input.code === 'native_request_resolved';
// Resolved requests leave the task view; only open ones and this tab's unfinished responses remain.
const visible = (input: Agent.PendingInput) => {
  const id = JSON.stringify(input.identity);
  return !resolved(input) && !completed.value.has(id) && responsePhase(id) !== 'succeeded' &&
    (['pending', 'answering', 'outcome_unknown'].includes(input.state) || unresolved(id));
};
const retained = ref(new Map<string, Agent.PendingInput>());
watch(observed, inputs => {
  for (const input of inputs) if (resolved(input)) {
    try { sessionStorage.removeItem(responseKey(JSON.stringify(input.identity)) + ':draft'); } catch { /* Optional view state. */ }
  }
  if (!props.retainDrafts) return;
  const next = new Map(inputs.filter(visible).map(input => [JSON.stringify(input.identity), input]));
  for (const [id, input] of retained.value) {
    if (observedIds.value.has(id) || next.size >= 256 || input.threadId !== props.threadId) continue;
    // Keep an unfinished response when a restarted owner drops its request.
    // It remains bound to the old identity and cannot be submitted against this snapshot.
    if (visible(input) && unresolved(id)) next.set(id, input);
  }
  retained.value = next;
});
const items = computed(() => (props.retainDrafts ? [...retained.value.values()] : observed.value).filter(visible));
const answerUpdated = (input: Agent.PendingInput) => {
  const id = JSON.stringify(input.identity);
  if (responsePhase(id) === 'succeeded') completed.value.add(id);
  void pending.refresh();
};
watch(() => items.value.filter(item => ['pending', 'answering', 'outcome_unknown'].includes(item.state)).length, count => emit('attention', count));
</script>
<template><section v-if="!hideEmpty || items.length || pending.error.value || pending.value.value?.truncated" class="space-y-4" aria-labelledby="native-input-heading"><div class="flex flex-wrap items-center justify-between gap-3"><h2 id="native-input-heading" class="text-lg font-semibold">Native questions and approvals</h2></div>
  <RemoteState :loading="pending.loading.value" :error="pending.error.value" :has-data="!!pending.value.value" @retry="pending.refresh" /><p v-if="pending.value.value && !items.length" class="text-sm text-muted-foreground">No retained request for this task in the current snapshot.</p>
  <p v-if="pending.value.value?.truncated" class="text-sm text-muted-foreground">The bounded owner snapshot is truncated; current pending inputs are ordered first.</p>
  <p v-if="ownerEpoch && pending.value.value?.epoch !== ownerEpoch" class="text-sm text-muted-foreground">This run belongs to an earlier native connection. Its retained requests are not answerable here.</p>
  <div v-for="input in items" :key="JSON.stringify(input.identity)" class="space-y-2"><p v-if="!observedIds.has(JSON.stringify(input.identity))" class="text-sm text-muted-foreground">This request is no longer in the current snapshot. Your response draft is retained.</p><NativeInput :base="base" :client="client" :input="input" :epoch="pending.value.value?.epoch ?? null" :available="observedIds.has(JSON.stringify(input.identity)) && !pending.error.value && (!ownerEpoch || pending.value.value?.epoch === ownerEpoch)" @answered="answerUpdated(input)" /></div>
</section></template>
