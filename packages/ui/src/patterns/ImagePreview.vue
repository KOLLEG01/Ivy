<script setup lang="ts">
import { ref, watch } from 'vue';
import { Download, Maximize2 } from '@lucide/vue';
import { Button } from '../components/button';
import { Dialog, DialogContent, DialogDescription, DialogTitle } from '../components/dialog';
const props = defineProps<{ source: string | null; name: string }>();
const open = defineModel<boolean>('open', { default: false });
const actualSize = ref(false);
watch(() => props.source, () => { actualSize.value = false; });
</script>
<template>
  <Dialog v-model:open="open">
    <DialogContent class="flex max-h-[95dvh] w-[95vw] flex-col sm:max-w-[95vw]">
      <DialogTitle class="break-all pr-8">{{ name || 'Image' }}</DialogTitle>
      <DialogDescription class="sr-only">Image preview. View at original size or download the image.</DialogDescription>
      <div class="flex flex-wrap gap-2">
        <Button variant="outline" size="sm" @click="actualSize = !actualSize"><Maximize2 aria-hidden="true" />{{ actualSize ? 'Fit to window' : 'Original size' }}</Button>
        <Button v-if="source" variant="outline" size="sm" as-child><a :href="source" :download="name || 'image'"><Download aria-hidden="true" />Download image</a></Button>
      </div>
      <div class="min-h-0 overflow-auto rounded-md bg-muted/30 p-2"><img v-if="source" :src="source" :alt="name" :class="actualSize ? 'max-w-none' : 'mx-auto max-h-[70dvh] max-w-full object-contain'" /></div>
    </DialogContent>
  </Dialog>
</template>
