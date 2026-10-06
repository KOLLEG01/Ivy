<script setup lang="ts">
import { computed, ref } from 'vue';
import { ArrowLeft, Download, FolderOpen } from '@lucide/vue';
import { Disclosure, Button, ContentView, Input, Label, PageControls, RemoteState, RevisionDiff, StatusBadge, useRemote } from '@ivy/ui';
import { downloadRevision } from '../../../../../packages/ui-client/src/content';
import { consoleClient, dateLabel, usePage } from '../runtime';
import type { Operation } from '../runtime';
const props = defineProps<{ query: URLSearchParams; link: (values: Record<string, string>) => string }>();
const emit = defineEmits<{ opened: [value: { id: string; name: string; parentId: string | null }] }>();
const objectId = props.query.get('id'), path = props.query.get('path'), revisionText = props.query.get('revision');
const revision = revisionText && /^[1-9]\d*$/.test(revisionText) ? Number(revisionText) : undefined;
const locator = objectId ? { objectId } : { path: path ?? '/' };
const detail = useRemote(async signal => {
  if (revisionText && !revision) throw new Error('This link has an invalid revision.');
  const result = await consoleClient.request('objects.read', { ...locator, ...(revision ? { revision } : {}) }, { signal });
  emit('opened', { id: result.object.id, name: result.object.name, parentId: result.object.parentId }); return result;
}, 0, revisionText ? undefined : ["objects"]);
const tab = computed(() => ['metadata', 'history'].includes(props.query.get('tab') ?? '') ? props.query.get('tab')! : 'content');
const history = usePage(async (signal, cursor) => consoleClient.request('objects.history', { objectId: objectId ?? (await consoleClient.request('objects.stat', locator, { signal })).id, limit: 50, ...(cursor ? { cursor } : {}) }, { signal }), undefined, ["objects"]);
const compareTo = ref(''), comparison = ref<Operation.ObjectRead | null>(null), compareBusy = ref(false), actionError = ref<string | null>(null);
const textOf = (value: Operation.ObjectRead | null) => !value ? '' : value.content.encoding === 'json' ? JSON.stringify(value.content.value, null, 2) : value.content.encoding === 'text' ? value.content.value : '(Binary content; download this exact revision to inspect it.)';
const compare = async () => {
  if (!detail.value.value || !/^[1-9]\d*$/.test(compareTo.value)) return;
  comparison.value = null; compareBusy.value = true; actionError.value = null;
  try { comparison.value = await consoleClient.request('objects.read', { objectId: detail.value.value.object.id, revision: Number(compareTo.value) }); }
  catch (error) { actionError.value = error instanceof Error ? error.message : 'Comparison unavailable.'; }
  finally { compareBusy.value = false; }
};
const download = async () => { if (!detail.value.value) return; actionError.value = null; try { await downloadRevision(detail.value.value); } catch (error) { actionError.value = error instanceof Error ? error.message : 'Download unavailable.'; } };
defineExpose({ refresh: () => { void detail.refresh(); history.reset(); comparison.value = null; } });
</script>
<template>
  <section aria-label="Object inspector" class="min-w-0">
    <div class="mb-3 flex items-center justify-between gap-2"><Button variant="ghost" size="sm" as-child><a :href="link({ id: '', path: '', revision: '', tab: '' })"><ArrowLeft aria-hidden="true" />Back to objects</a></Button><Button v-if="detail.value.value" variant="ghost" size="icon" aria-label="Download revision" title="Download revision" @click="download"><Download aria-hidden="true" /></Button></div>
    <RemoteState :loading="detail.loading.value" :error="detail.error.value" :has-data="!!detail.value.value" @retry="detail.refresh" />
    <template v-if="detail.value.value">
      <header class="mb-4"><h2 class="break-words text-xl font-semibold">{{ detail.value.value.object.name }}</h2><p class="mt-1 break-all text-xs text-muted-foreground">{{ detail.value.value.object.contractKey }}</p><div class="mt-2 flex flex-wrap items-center gap-2"><StatusBadge v-if="detail.value.value.object.effectivelyArchived" label="Archived" /><StatusBadge v-if="revision" :label="'Revision ' + revision + ' · Historical'" tone="warning" /><Button variant="ghost" size="sm" as-child><a :href="link({ parent: detail.value.value.object.id, id: '', path: '', revision: '', tab: '' })"><FolderOpen aria-hidden="true" />Browse children</a></Button><a v-if="revision" :href="link({ revision: '', tab: 'content' })" class="text-xs underline">Current revision</a></div></header>
      <nav aria-label="Inspector views" class="workspace-tabs mb-5 border-b"><a v-for="[value, label] in [['content', 'Content'], ['metadata', 'Metadata'], ['history', 'History']]" :key="value" :href="link({ tab: value! })" :aria-current="tab === value ? 'page' : undefined">{{ label }}</a></nav>
      <ContentView v-if="tab === 'content'" :text="textOf(detail.value.value)" :media-type="detail.value.value.revision.mediaType" />
      <section v-else-if="tab === 'metadata'" aria-label="Identity and revision metadata"><dl class="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-3 text-sm"><dt class="text-muted-foreground">Path</dt><dd class="break-all">{{ detail.value.value.object.path }}</dd><dt class="text-muted-foreground">ID</dt><dd class="break-all font-mono text-xs">{{ detail.value.value.object.id }}</dd><dt class="text-muted-foreground">Version</dt><dd>{{ detail.value.value.revision.contractVersion }}</dd><dt class="text-muted-foreground">Revision</dt><dd>{{ detail.value.value.revision.revision }}</dd><dt class="text-muted-foreground">Size</dt><dd>{{ detail.value.value.revision.byteLength.toLocaleString() }} bytes</dd></dl><Disclosure class="mt-6" title="Raw metadata"><ContentView class="mt-3" :text="JSON.stringify({ object: detail.value.value.object, revision: detail.value.value.revision }, null, 2)" /></Disclosure></section>
      <section v-else aria-labelledby="revision-heading"><h3 id="revision-heading" class="sr-only">Revision history</h3><RemoteState :loading="history.loading.value" :error="history.error.value" :has-data="!!history.value.value" @retry="history.refresh" />
        <div class="divide-y"><a v-for="item in history.value.value?.items ?? []" :key="item.revision" :href="link({ id: item.objectId, revision: String(item.revision), tab: 'content' })" class="flex items-center justify-between gap-3 py-3 text-sm hover:bg-muted"><span><strong class="font-medium">Revision {{ item.revision }}</strong><span class="mt-1 block text-xs text-muted-foreground">{{ dateLabel(item.createdAt) }}</span></span><span class="text-xs text-muted-foreground">{{ item.contractVersion }} · {{ item.byteLength.toLocaleString() }} B</span></a></div>
        <PageControls v-if="history.value.value && (history.page.value > 1 || history.value.value.nextCursor)" :count="history.value.value.items.length" :page="history.page.value" :has-next="!!history.value.value.nextCursor" :loading="history.loading.value" @next="history.next" @previous="history.previous" />
        <form class="mt-5 flex flex-wrap items-end gap-3" @submit.prevent="compare"><div class="w-40"><Label for="compare-revision">Compare with revision</Label><Input id="compare-revision" v-model="compareTo" type="number" min="1" :max="detail.value.value.object.currentRevision" class="mt-2" required /></div><Button variant="outline" type="submit" :disabled="compareBusy || detail.value.value.content.encoding === 'base64'">Compare content</Button></form>
        <RevisionDiff v-if="comparison" class="mt-5" :before="textOf(comparison)" :after="textOf(detail.value.value)" :before-label="'Revision ' + comparison.revision.revision" :after-label="'Revision ' + detail.value.value.revision.revision" />
      </section>
    </template>
    <p v-if="actionError" class="mt-4 rounded-lg border p-4 text-sm" role="alert">{{ actionError }}</p>
  </section>
</template>
