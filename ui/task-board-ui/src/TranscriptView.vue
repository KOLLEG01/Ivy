<script setup lang="ts">
import { ref } from 'vue';
import { Button, ContentView, RemoteState, useRemote } from '@ivy/ui';
import { readNativeOutput } from './native-output';
import { list, nativeText, record } from '../../../packages/ui-client/src/native';
import { readDocument } from './runtime';
import type { TaskBoard } from './runtime';
const props = defineProps<{ artifact: TaskBoard.Artifact }>();
const pins = ref<TaskBoard.ObjectPin[]>([]), selected = ref(0);
const state = useRemote(async signal => {
  const transcript = await readDocument('native-transcript', props.artifact.object, undefined, signal);
  if (transcript.read.revision.contentHash !== props.artifact.contentHash) throw new Error('The saved transcript pin changed.');
  if (!transcript.value.head) return { transcript, page: null, items: [], raw: '' };
  if (!pins.value.length) pins.value = [transcript.value.head];
  const page = await readDocument('native-result-page', pins.value[selected.value]!, transcript.value.run.objectId, signal);
  if (page.value.collectionId !== transcript.value.collectionId || page.value.runId !== transcript.value.run.objectId || page.value.index !== transcript.value.pageCount - selected.value) throw new Error('The selected page is outside this saved transcript chain.');
  const { raw, observed } = await readNativeOutput(transcript.value.run.objectId, page.value.evidence, 'thread/items/list', signal);
  const items = list(record(record(observed.reply).result).data).map(item => record(record(item).item ?? item));
  if (items.length !== page.value.itemCount) throw new Error('The native item count differs from its saved page.');
  return { transcript, page, items, raw };
});
const older = () => { if (!state.value.value?.page?.value.previous) return; pins.value[selected.value + 1] = state.value.value.page.value.previous; selected.value++; void state.refresh(); };
const newer = () => { if (selected.value === 0) return; selected.value--; void state.refresh(); };
const download = () => { if (!state.value.value?.raw) return; const url = URL.createObjectURL(new Blob([state.value.value.raw], { type: 'application/octet-stream' })), anchor = document.createElement('a'); anchor.href = url; anchor.download = 'native-output-page-' + (state.value.value.page?.value.index ?? 1) + '.json'; anchor.click(); setTimeout(() => URL.revokeObjectURL(url), 1000); };
</script>
<template><section class="space-y-4 rounded-lg border p-4"><div class="flex flex-wrap items-center justify-between gap-3"><h4 class="font-semibold">Saved native output</h4><Button variant="outline" size="sm" :disabled="!state.value.value?.raw" @click="download">Download full output page</Button></div>
  <RemoteState :loading="state.loading.value" :error="state.error.value" :has-data="!!state.value.value" @retry="state.refresh" />
  <template v-if="state.value.value"><p class="text-xs text-muted-foreground">{{ state.value.value.transcript.value.phase }} transcript · {{ state.value.value.transcript.value.nativeBytes.toLocaleString() }} bytes · {{ state.value.value.transcript.value.pageCount }} pages. The most recent saved page opens first.</p><p v-if="state.value.value.transcript.value.phase !== 'complete'" class="text-sm">Collection is incomplete. This is retained intermediate output.</p>
    <article v-for="(item, index) in state.value.value.items" :key="index" class="space-y-2 border-t pt-3"><p class="text-xs font-medium text-muted-foreground">{{ item.type }}</p><div class="max-h-[30rem] overflow-auto"><ContentView :text="nativeText(item) || JSON.stringify(item, null, 2)" :maximum-characters="16384" :media-type="item.type === 'agentMessage' ? 'text/markdown' : 'text/plain'" /></div></article>
    <div class="flex flex-wrap items-center gap-3"><Button variant="outline" size="sm" :disabled="state.loading.value || !state.value.value.page?.value.previous" @click="older">Older output page</Button><span class="text-xs text-muted-foreground">Page {{ state.value.value.page?.value.index ?? 1 }} of {{ state.value.value.transcript.value.pageCount }}</span><Button variant="outline" size="sm" :disabled="state.loading.value || selected === 0" @click="newer">Newer output page</Button></div>
  </template>
</section></template>
