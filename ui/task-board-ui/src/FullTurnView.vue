<script setup lang="ts">
import { computed, ref } from 'vue';
import { Button, ContentView, RemoteState, useRemote } from '@ivy/ui';
import { list, nativeText, record } from '../../../packages/ui-client/src/native';
import { readDocument } from './runtime';
import type { TaskBoard } from './runtime';
import { readNativeOutput } from './native-output';
const props = defineProps<{ artifact: TaskBoard.Artifact }>();
const selected = ref(0);
const state = useRemote(async signal => {
  const transcript = await readDocument('native-full-turn-transcript', props.artifact.object, undefined, signal), value = transcript.value;
  if (transcript.read.revision.contentHash !== props.artifact.contentHash || transcript.read.object.parentId !== value.run.objectId) throw new Error('The saved transcript pin changed.');
  const snapshot = await readDocument('native-turn-snapshot', value.snapshot.object, value.run.objectId, signal);
  if (snapshot.read.revision.contentHash !== value.snapshot.contentHash || snapshot.value.run.objectId !== value.run.objectId) throw new Error('The original saved turn differs from its snapshot.');
  const output = await readNativeOutput(value.run.objectId, value.fullTurn, 'thread/turns/list', signal), params = record(output.observed.params);
  if (output.bytes !== value.fullTurnBytes || output.observed.epoch !== snapshot.value.epoch || params.threadId !== snapshot.value.threadId || params.limit !== 1 || params.itemsView !== 'full' || params.sortDirection !== 'desc' || params.cursor !== value.fullTurnCursor) throw new Error('The saved full output identifies another native read.');
  const turns = list(record(record(output.observed.reply).result).data), turn = record(turns[0]);
  if (turns.length !== 1 || turn.id !== snapshot.value.turnId || turn.status !== snapshot.value.status || turn.itemsView !== 'full' || !Array.isArray(turn.items)) throw new Error('The saved native turn is incomplete or changed.');
  const items = list(turn.items).map(record), ids = new Set<string>();
  for (const item of items) { if (typeof item.id !== 'string' || !item.id || ids.has(item.id)) throw new Error('The saved native items have invalid identities.'); ids.add(item.id); }
  return { ...output, items };
});
const visible = computed(() => state.value.value?.items.slice(selected.value * 50, (selected.value + 1) * 50) ?? []);
const download = () => { if (!state.value.value) return; const url = URL.createObjectURL(new Blob([state.value.value.raw], { type: 'application/octet-stream' })), anchor = document.createElement('a'); anchor.href = url; anchor.download = 'native-full-turn.json'; anchor.click(); setTimeout(() => URL.revokeObjectURL(url), 1000); };
</script>
<template><section class="space-y-4 rounded-lg border p-4"><div class="flex flex-wrap items-center justify-between gap-3"><h4 class="font-semibold">Saved native output</h4><Button variant="outline" size="sm" :disabled="!state.value.value" @click="download">Download full output</Button></div>
  <RemoteState :loading="state.loading.value" :error="state.error.value" :has-data="!!state.value.value" @retry="state.refresh" />
  <template v-if="state.value.value"><p class="text-xs text-muted-foreground">Complete saved turn · {{ state.value.value.bytes.toLocaleString() }} bytes · {{ state.value.value.items.length }} items</p>
    <div class="max-h-[40rem] space-y-4 overflow-auto" role="region" aria-label="Saved native items" tabindex="0"><article v-for="item in visible" :key="String(item.id)" class="space-y-2 border-t pt-3"><p class="text-xs font-medium text-muted-foreground">{{ item.type }}</p><div class="max-h-[30rem] overflow-auto"><ContentView :text="nativeText(item) || JSON.stringify(item, null, 2)" :maximum-characters="16384" :media-type="item.type === 'agentMessage' ? 'text/markdown' : 'text/plain'" /></div></article></div>
    <div v-if="state.value.value.items.length > 50" class="flex flex-wrap items-center gap-3"><Button variant="outline" size="sm" :disabled="selected === 0" @click="selected--">Previous items</Button><span class="text-xs text-muted-foreground">Items {{ selected * 50 + 1 }}–{{ Math.min((selected + 1) * 50, state.value.value.items.length) }}</span><Button variant="outline" size="sm" :disabled="(selected + 1) * 50 >= state.value.value.items.length" @click="selected++">Next items</Button></div>
  </template>
</section></template>
