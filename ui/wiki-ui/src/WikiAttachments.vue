<script setup lang="ts">
import { computed, ref } from 'vue';
import { Download, FileText, Paperclip, Plus, Trash2, Upload } from '@lucide/vue';
import { Alert, AlertDescription, Button, Input, Label, PageControls, RemoteState, useRemote } from '@ivy/ui';
import type { UploadedAttachment } from '@ivy/ui';
import { route, usePage } from '../../../packages/ui-client/src/runtime';
import { mapConcurrent } from "../../../packages/ui-client/src/concurrency";
import type { Operation } from '../../../packages/ui-client/src/runtime';
import { downloadRevision } from '../../../packages/ui-client/src/content';
import ObjectDeleteDialog from '../../../packages/ui-client/src/ObjectDeleteDialog.vue';
import { base, client } from './runtime';
import type { useWikiUpload } from './wiki-upload';

const props = defineProps<{
  pageId: string; writable: boolean; historical: boolean;
  references: Operation.ObjectRead['revision']['references'];
  imageUrls: Record<string, string>;
  uploader: ReturnType<typeof useWikiUpload>;
  uploadFile: (file: File) => Promise<UploadedAttachment>;
}>();
const emit = defineEmits<{ uploaded: [files: UploadedAttachment[]]; insert: [file: Operation.ObjectRead] }>();
const children = usePage((signal, cursor) => client.request('objects.list', { parentId: props.pageId, limit: 50, ...(cursor ? { cursor } : {}) }, { signal }), 0, undefined, props.historical ? undefined : ["objects/wiki/page", "objects/wiki/attachment"]);
const pastFiles = useRemote(async (signal) => {
  if (!props.historical) return [];
  return mapConcurrent(
    Object.entries(props.references).filter(([name]) => name.startsWith('attachment.')),
    async ([, reference]) => {
    const item = await client.request('objects.stat', { objectId: reference.objectId }, { signal });
    return { ...item, currentRevision: reference.revision };
  },
    signal,
  );
});
const selected = ref<File[]>([]), choosing = ref(false), action = ref<string | null>(null);
const deleteTarget = ref<Operation.ObjectRead['object'] | null>(null), deleteOpen = ref(false);
const pages = computed(() => props.historical ? [] : children.value.value?.items.filter(item => item.contractKey === 'wiki/page') ?? []);
const attachments = computed(() => props.historical ? pastFiles.value.value ?? [] : children.value.value?.items.filter(item => item.contractKey === 'wiki/attachment') ?? []);
const pending = computed(() => props.uploader.pending.value), busy = computed(() => props.uploader.busy.value || choosing.value || !!action.value);
const choose = async (event: Event) => {
  selected.value = []; props.uploader.setError(null);
  const input = event.target as HTMLInputElement, files = Array.from(input.files ?? []);
  input.value = '';
  if (!files.length) return;
  choosing.value = true;
  try {
    if (pending.value && files.length !== 1) throw new Error('Reselect only the original file to retry this upload.');
    for (const file of files) await props.uploader.validate(file);
    selected.value = files;
  } catch (cause) { props.uploader.setError(cause instanceof Error ? cause.message : 'The files could not be read.'); }
  finally { choosing.value = false; }
};
const upload = async () => {
  if (!selected.value.length || busy.value || (!props.writable && !pending.value)) return;
  const files = [...selected.value], added: UploadedAttachment[] = [];
  action.value = 'upload'; props.uploader.setError(null);
  try {
    for (const file of files) {
      added.push(await props.uploadFile(file));
      selected.value = selected.value.slice(1);
    }
  } catch (cause) { props.uploader.setError(cause instanceof Error ? cause.message : 'Upload could not be confirmed.'); }
  finally {
    action.value = null;
    if (added.length) emit('uploaded', added);
    void children.refresh();
  }
};
const useFile = async (item: Operation.ObjectRead['object'], insert: boolean) => {
  action.value = item.id; props.uploader.setError(null);
  try {
    const value = await client.request('objects.read', { objectId: item.id, revision: item.currentRevision });
    if (value.object.parentId !== props.pageId || value.object.ownerObjectId !== props.pageId) throw new Error('This attachment is no longer owned by this page.');
    if (insert) emit('insert', value); else await downloadRevision(value);
  } catch (cause) { props.uploader.setError(cause instanceof Error ? cause.message : 'The attachment is unavailable.'); }
  finally { action.value = null; }
};
const remove = (item: Operation.ObjectRead['object']) => { deleteTarget.value = item; deleteOpen.value = true; };
const deleted = () => { deleteTarget.value = null; void children.refresh(); window.dispatchEvent(new Event('ivy:objects-changed')); };
</script>
<template>
  <section class="space-y-4 pt-2" aria-labelledby="children-heading">
    <div class="flex items-center justify-between gap-3"><h3 id="children-heading" class="text-sm font-medium">{{ historical ? 'Files in this revision' : 'Subpages and files' }}</h3><Button v-if="writable" variant="ghost" size="sm" as-child><a :href="route('new', { parent: pageId })"><Plus aria-hidden="true" />Add child page</a></Button></div>
    <RemoteState v-if="historical" :loading="pastFiles.loading.value" :error="pastFiles.error.value" :has-data="!!pastFiles.value.value" :empty="pastFiles.value.value?.length === 0" empty-title="No files in this revision" @retry="pastFiles.refresh" />
    <RemoteState v-else :loading="children.loading.value" :error="children.error.value" :has-data="!!children.value.value" :empty="children.value.value?.items.length === 0" empty-title="No subpages or files" @retry="children.refresh" />
    <div v-if="pages.length || attachments.length" class="overflow-hidden rounded-lg border">
      <a v-for="page in pages" :key="page.id" :href="route('page', { id: page.id })" class="flex items-center gap-2 border-b px-3 py-2 text-sm last:border-b-0 hover:bg-muted/50"><FileText class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" /><span class="min-w-0 truncate">{{ page.name }}</span></a>
      <div v-for="item in attachments" :key="item.id" class="space-y-2 border-b p-3 last:border-b-0">
        <a :href="route('page', { id: item.id, revision: String(item.currentRevision) })" class="flex items-start gap-2 text-sm underline-offset-4 hover:underline"><img v-if="imageUrls[route('page', { id: item.id, revision: String(item.currentRevision) })]" :src="imageUrls[route('page', { id: item.id, revision: String(item.currentRevision) })]" alt="" class="size-12 shrink-0 rounded-md object-cover" /><Paperclip v-else class="size-4 shrink-0 text-muted-foreground" aria-hidden="true" /><span class="min-w-0 flex-1 break-all">{{ item.name }}</span></a>
        <div class="flex flex-wrap items-center gap-1">
          <Button v-if="writable" variant="outline" size="sm" :disabled="busy" :aria-label="'Insert ' + item.name" @click="useFile(item, true)"><Plus aria-hidden="true" />Insert</Button>
          <Button variant="ghost" size="sm" :disabled="busy" :aria-label="'Download ' + item.name" @click="useFile(item, false)"><Download aria-hidden="true" />Download</Button>
          <Button v-if="writable" variant="ghost" size="icon-sm" :disabled="busy || !!references['attachment.' + item.id]" :aria-label="'Delete ' + item.name" @click="remove(item)"><Trash2 aria-hidden="true" /></Button>
          <span class="ml-auto text-xs text-muted-foreground">Revision {{ item.currentRevision }}</span>
        </div>
      </div>
    </div>
    <PageControls v-if="!historical && children.value.value && (children.value.value.nextCursor || children.page.value > 1)" :count="children.value.value.items.length" :page="children.page.value" :has-next="!!children.value.value.nextCursor" :loading="children.loading.value" @next="children.next" @previous="children.previous" />
    <div v-if="writable || pending" class="space-y-2 border-t pt-4"><Label for="attachment-file">Choose attachment</Label><Input id="attachment-file" type="file" :multiple="!pending" :disabled="busy" @change="choose" /><p class="text-xs text-muted-foreground">Images and files are inserted into your draft. Save the page to publish them. Up to 8 MiB per file; duplicate names receive a number.</p><p class="text-xs text-muted-foreground">PNG, JPEG, GIF and WebP have an image preview. Other formats appear as downloadable files.</p>
      <p v-if="pending" class="text-sm" role="status">The upload of {{ pending.originalName ?? pending.name }} is unconfirmed. Reselect the original file after a reload to retry without creating a duplicate.</p>
      <p v-if="selected.length" class="break-all text-xs text-muted-foreground">{{ selected.map(file => file.name).join(', ') }}</p>
      <Button size="sm" variant="outline" :disabled="!selected.length || busy" @click="upload"><Upload aria-hidden="true" />{{ busy ? 'Working…' : pending ? 'Retry original upload' : 'Upload attachment' }}</Button>
    </div>
    <Alert v-if="uploader.error.value" variant="destructive"><AlertDescription>{{ uploader.error.value }}</AlertDescription></Alert>
    <p v-if="attachments.length && !historical" class="text-xs text-muted-foreground">Remove an image or file link in the editor to remove its embed. Files used by saved revisions are kept for page history.</p>
    <ObjectDeleteDialog v-if="deleteTarget" v-model:open="deleteOpen" :client="client" :object-id="deleteTarget.id" :expected-revision="deleteTarget.currentRevision" :label="'the file “' + deleteTarget.name + '”'" consequence="Files used by saved page revisions cannot be deleted separately." :storage-key="'ivy.wiki.delete-file:' + base.href + ':' + deleteTarget.id" @deleted="deleted" />
  </section>
</template>
