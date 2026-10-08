<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue';
import { markdownImageSources, renderMarkdown } from '../lib/markdown';
import type { AttachmentInfo } from '../lib/attachments';
import { Button } from '../components/button';
import ImagePreview from './ImagePreview.vue';
const props = defineProps<{ text: string; mediaType?: string; maximumCharacters?: number; trustedImageUrls?: Record<string, string>; trustedAttachments?: Record<string, AttachmentInfo>; imageLoader?: ((source: string, signal: AbortSignal) => Promise<Blob | null>) | null }>();
const limit = computed(() => props.maximumCharacters ?? 262144);
const loaded = ref<Record<string, string>>({}), failures = ref<string[]>([]);
const imageUrls = computed(() => ({ ...loaded.value, ...props.trustedImageUrls }));
const markdown = computed(() => props.mediaType === 'text/markdown' ? renderMarkdown(props.text, limit.value, imageUrls.value, props.trustedAttachments) : null);
let controller = new AbortController();
const attempted = new Set<string>();
const releaseImages = () => {
  controller.abort();
  for (const url of Object.values(loaded.value)) URL.revokeObjectURL(url);
  loaded.value = {};
  attempted.clear();
  failures.value = [];
  controller = new AbortController();
};
const loadImages = async () => {
  const loader = props.imageLoader, signal = controller.signal;
  if (!loader || props.mediaType !== 'text/markdown') return;
  for (const source of markdownImageSources(props.text, limit.value)) {
    if (attempted.has(source) || imageUrls.value[source]) continue;
    attempted.add(source);
    try {
      const image = await loader(source, signal);
      if (signal.aborted) return;
      if (image) loaded.value = { ...loaded.value, [source]: URL.createObjectURL(image) };
    } catch {
      if (signal.aborted) return;
      failures.value = [...failures.value, source];
    }
  }
};
const retryImages = () => {
  for (const source of failures.value) attempted.delete(source);
  failures.value = [];
  void loadImages();
};
watch(() => props.imageLoader, () => { releaseImages(); void loadImages(); }, { immediate: true });
watch(() => [props.text, props.mediaType, limit.value], () => void loadImages());
onBeforeUnmount(releaseImages);
const previewOpen = ref(false), preview = ref({ source: '', name: '' });
const showImage = (event: MouseEvent) => {
  const button = event.target instanceof Element ? event.target.closest('button.ivy-image-button') : null;
  const image = button?.querySelector('img');
  if (image && Object.values(imageUrls.value).includes(image.src)) { event.preventDefault(); preview.value = { source: image.src, name: image.alt }; previewOpen.value = true; }
};
</script>
<template>
  <div>
    <p v-if="text.length > limit" class="mb-3 rounded-md border px-3 py-2 text-sm text-muted-foreground" role="status">Preview limited to {{ limit.toLocaleString() }} characters. Download the saved content to read it in full.</p>
    <!-- Only the local HTML-disabled Markdown renderer supplies this HTML. -->
    <div v-if="markdown" class="ivy-markdown" @click="showImage" v-html="markdown.html" />
    <pre v-else class="max-h-[38rem] overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted/60 p-4 font-mono text-xs leading-relaxed">{{ text.slice(0, limit) }}</pre>
    <p v-if="failures.length" class="mt-2 text-sm text-muted-foreground" role="status">Some images could not be loaded. <Button variant="link" size="sm" @click="retryImages">Retry images</Button></p>
  </div>
  <ImagePreview v-model:open="previewOpen" :source="preview.source" :name="preview.name" />
</template>
<style scoped>
:deep(.ivy-image-button) { display: block; max-width: 100%; }
:deep(.ivy-embedded-image) { display: block; max-width: 100%; height: auto; max-height: 42rem; margin: .75rem 0; border-radius: .375rem; }
</style>
