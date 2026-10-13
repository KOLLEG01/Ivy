<script setup lang="ts">
import { ref, watch } from 'vue';
import { ChevronLeft, ChevronRight, Download, Maximize2 } from '@lucide/vue';
import { Button } from '../components/button';
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '../components/dialog';
const props = withDefaults(defineProps<{
  source: string | null;
  name: string;
  index?: number;
  count?: number;
  loading?: boolean;
  error?: string | null;
  previousLabel?: string;
  nextLabel?: string;
}>(), { index: 0, count: 1, previousLabel: 'Previous image', nextLabel: 'Next image' });
const emit = defineEmits<{ previous: []; next: []; retry: [] }>();
const open = defineModel<boolean>('open', { default: false });
const actualSize = ref(false);
watch(() => props.source, () => { actualSize.value = false; });
const navigate = (event: KeyboardEvent) => {
  if (event.altKey || event.ctrlKey || event.metaKey || props.count <= 1) return;
  if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
    event.preventDefault();
    if (event.key === 'ArrowLeft' && props.index > 0) emit('previous');
    if (event.key === 'ArrowRight' && props.index < props.count - 1) emit('next');
  }
};
</script>
<template>
  <Dialog v-model:open="open">
    <DialogContent class="flex max-h-[95dvh] w-[95vw] flex-col sm:max-w-[95vw]" @keydown="navigate">
      <DialogTitle class="break-all pr-8">{{ name || 'Image' }}</DialogTitle>
      <DialogDescription class="sr-only">Image preview. View at original size or download the image.</DialogDescription>
      <div v-if="count > 1" class="flex items-center justify-center gap-3">
        <Button variant="outline" size="icon-sm" :aria-label="previousLabel" :disabled="index <= 0" @click="emit('previous')"><ChevronLeft aria-hidden="true" /></Button>
        <span class="text-sm tabular-nums text-muted-foreground" aria-live="polite">{{ index + 1 }} / {{ count }}</span>
        <Button variant="outline" size="icon-sm" :aria-label="nextLabel" :disabled="index >= count - 1" @click="emit('next')"><ChevronRight aria-hidden="true" /></Button>
      </div>
      <div class="flex flex-wrap gap-2">
        <Button variant="outline" size="sm" :disabled="!source" @click="actualSize = !actualSize"><Maximize2 aria-hidden="true" />{{ actualSize ? 'Fit to window' : 'Original size' }}</Button>
        <Button v-if="source" variant="outline" size="sm" as-child><a :href="source" :download="name || 'image'"><Download aria-hidden="true" />Download image</a></Button>
      </div>
      <p v-if="loading" role="status" class="text-sm text-muted-foreground">Loading image…</p>
      <div v-if="error" class="space-y-2"><p role="alert" class="text-sm text-destructive">{{ error }}</p><Button variant="outline" size="sm" @click="emit('retry')">Retry image</Button></div>
      <div class="min-h-0 overflow-auto rounded-md bg-muted/30 p-2"><img v-if="source" :src="source" :alt="name" :class="actualSize ? 'max-w-none' : 'mx-auto max-h-[70dvh] max-w-full object-contain'" /></div>
    </DialogContent>
  </Dialog>
</template>
