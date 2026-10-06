<script setup lang="ts">
import { computed, ref } from 'vue';
import { renderMarkdown } from '../lib/markdown';
import type { AttachmentInfo } from '../lib/attachments';
import ImagePreview from './ImagePreview.vue';
const props = defineProps<{ text: string; mediaType?: string; maximumCharacters?: number; trustedImageUrls?: Record<string, string>; trustedAttachments?: Record<string, AttachmentInfo> }>();
const limit = computed(() => props.maximumCharacters ?? 262144);
const markdown = computed(() => props.mediaType === 'text/markdown' ? renderMarkdown(props.text, limit.value, props.trustedImageUrls, props.trustedAttachments) : null);
const previewOpen = ref(false), preview = ref({ source: '', name: '' });
const showImage = (event: MouseEvent) => {
  const button = event.target instanceof Element ? event.target.closest('button.ivy-image-button') : null;
  const image = button?.querySelector('img');
  if (image && Object.values(props.trustedImageUrls ?? {}).includes(image.src)) { event.preventDefault(); preview.value = { source: image.src, name: image.alt }; previewOpen.value = true; }
};
</script>
<template>
  <div>
    <p v-if="text.length > limit" class="mb-3 rounded-md border px-3 py-2 text-sm text-muted-foreground" role="status">Preview limited to {{ limit.toLocaleString() }} characters. Download the saved content to read it in full.</p>
    <!-- Only the local HTML-disabled Markdown renderer supplies this HTML. -->
    <div v-if="markdown" class="ivy-markdown" @click="showImage" v-html="markdown.html" />
    <pre v-else class="max-h-[38rem] overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted/60 p-4 font-mono text-xs leading-relaxed">{{ text.slice(0, limit) }}</pre>
  </div>
  <ImagePreview v-model:open="previewOpen" :source="preview.source" :name="preview.name" />
</template>
<style scoped>
:deep(.ivy-embedded-image) { display: block; max-width: 100%; height: auto; max-height: 42rem; margin: .75rem 0; border-radius: .375rem; }
</style>
