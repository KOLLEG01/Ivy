<script setup lang="ts">
import { ref, watch } from 'vue';
import { Button, ContentView, RemoteState, StatusBadge, useRemote } from '@ivy/ui';
import { downloadRevision } from '../../../packages/ui-client/src/content';
import { client, consoleObject, readDocument, statusTone } from './runtime';
import type { TaskBoard } from './runtime';
import TranscriptView from './TranscriptView.vue';
import FullTurnView from './FullTurnView.vue';
const props = defineProps<{ pin: TaskBoard.ObjectPin; taskId: string; root: string | null }>();
const state = useRemote(async signal => { const result = await readDocument('result', props.pin, props.root, signal); if (result.value.taskId !== props.taskId) throw new Error('The saved result belongs to another task.'); return result; });
watch(() => props.pin, () => { void state.refresh(); });
const artifactError = ref<string | null>(null), transcript = ref<TaskBoard.Artifact | null>(null);
const fullTurn = ref(false);
const open = async (artifact: TaskBoard.Artifact) => {
  artifactError.value = null;
  try {
    const read = await client.request('objects.read', artifact.object);
    if (read.revision.contentHash !== artifact.contentHash || read.revision.mediaType !== artifact.mediaType) throw new Error('The artifact differs from its saved evidence pin.');
    if (['task-board/native-transcript', 'task-board/native-full-turn-transcript'].includes(read.object.contractKey)) { fullTurn.value = read.object.contractKey === 'task-board/native-full-turn-transcript'; transcript.value = artifact; }
    else await downloadRevision(read);
  } catch (cause) { artifactError.value = cause instanceof Error ? cause.message : 'The artifact could not be opened.'; }
};
const download = async () => { try { if (state.value.value) await downloadRevision(state.value.value.read); } catch (cause) { artifactError.value = cause instanceof Error ? cause.message : 'The saved result could not be downloaded.'; } };
</script>
<template><section class="space-y-4"><RemoteState :loading="state.loading.value" :error="state.error.value" :has-data="!!state.value.value" @retry="state.refresh" />
  <template v-if="state.value.value"><div class="flex flex-wrap items-center gap-3"><h3 class="text-base font-semibold">Saved {{ state.value.value.value.kind }} result</h3><span class="text-xs text-muted-foreground">{{ state.value.value.value.createdAt }} · {{ state.value.value.value.actor.principalId }}</span></div>
    <Button type="button" variant="outline" size="sm" @click="download">Download saved result</Button>
    <div class="max-h-[30rem] overflow-auto"><ContentView :text="state.value.value.value.content.summary" media-type="text/markdown" :maximum-characters="8192" /></div>
    <div v-if="state.value.value.value.content.artifacts.length" class="space-y-2"><h4 class="font-semibold">Artifacts</h4><div v-for="artifact in state.value.value.value.content.artifacts" :key="JSON.stringify(artifact.object)" class="flex flex-wrap items-center gap-3 rounded-lg border p-3"><Button variant="outline" size="sm" @click="open(artifact)">{{ artifact.label }}</Button><a :href="consoleObject(artifact.object)" class="text-xs text-muted-foreground underline">Inspect saved revision {{ artifact.object.revision }}</a></div></div>
    <p v-if="artifactError" role="alert" class="text-sm text-destructive">{{ artifactError }}</p><FullTurnView v-if="transcript && fullTurn" :key="JSON.stringify(transcript.object)" :artifact="transcript" /><TranscriptView v-else-if="transcript" :key="JSON.stringify(transcript.object)" :artifact="transcript" />
    <section v-if="state.value.value.value.content.checks.length" class="space-y-3"><h4 class="font-semibold">Check evidence</h4><article v-for="(check, index) in state.value.value.value.content.checks" :key="index" class="space-y-2 rounded-lg border p-4"><div class="flex flex-wrap items-center gap-3"><strong>{{ check.name }}</strong><StatusBadge :label="check.status" :tone="statusTone(check.status)" /></div><p class="whitespace-pre-wrap text-sm">{{ check.detail }}</p><Button v-for="artifact in check.evidence" :key="JSON.stringify(artifact.object)" variant="outline" size="sm" @click="open(artifact)">{{ artifact.label }}</Button></article></section>
    <section v-if="state.value.value.value.content.repositoryResult" class="space-y-3 rounded-lg border p-4"><div class="flex flex-wrap items-center gap-3"><h4 class="font-semibold">Git repository result</h4><StatusBadge :label="state.value.value.value.content.repositoryResult.originState.replaceAll('_', ' ')" :tone="statusTone(state.value.value.value.content.repositoryResult.originState)" /></div><dl class="grid gap-3 text-sm sm:grid-cols-2"><div><dt class="text-muted-foreground">Repository</dt><dd>{{ state.value.value.value.content.repositoryResult.repositoryName }}</dd></div><div><dt class="text-muted-foreground">Execution PC</dt><dd>{{ state.value.value.value.content.repositoryResult.hostId }}</dd></div><div><dt class="text-muted-foreground">Branch</dt><dd>{{ state.value.value.value.content.repositoryResult.branch ?? 'Detached HEAD' }}</dd></div><div><dt class="text-muted-foreground">Commit</dt><dd class="break-all font-mono text-xs">{{ state.value.value.value.content.repositoryResult.commit ?? 'Unavailable' }}</dd></div><div><dt class="text-muted-foreground">Origin</dt><dd class="break-all">{{ state.value.value.value.content.repositoryResult.originUrl ?? 'No origin' }}</dd></div><div><dt class="text-muted-foreground">Observed remote ref</dt><dd class="break-all">{{ state.value.value.value.content.repositoryResult.remoteRef ?? 'None' }}</dd></div></dl><p v-if="state.value.value.value.content.repositoryResult.limitation" class="text-sm text-muted-foreground">{{ state.value.value.value.content.repositoryResult.limitation }}</p></section>
    <div v-if="state.value.value.value.content.codeReferences.length" class="space-y-2"><h4 class="font-semibold">Code and review links</h4><a v-for="link in state.value.value.value.content.codeReferences" :key="link.url" :href="/^https?:\/\//.test(link.url) ? link.url : undefined" target="_blank" rel="noopener noreferrer" class="block break-all text-sm underline">{{ link.label }} · {{ link.kind }}{{ link.commit ? ' · ' + link.commit : '' }}</a></div>
    <div v-if="state.value.value.value.content.limitations.length" class="space-y-2"><h4 class="font-semibold">Limitations</h4><ul class="list-disc space-y-1 pl-5 text-sm"><li v-for="(limit, index) in state.value.value.value.content.limitations" :key="index">{{ limit }}</li></ul></div>
  </template>
</section></template>
