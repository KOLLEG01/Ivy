<script setup lang="ts">
import { Reply, X } from '@lucide/vue';
import { Button } from '../components/button';

defineProps<{ text: string; label: string; jumpLabel: string; cancelLabel: string }>();
const emit = defineEmits<{ jump: []; cancel: [] }>();
</script>

<template>
  <div data-slot="reply-context" class="flex min-w-0 items-center gap-1 rounded-full bg-muted pr-1 text-xs">
    <button type="button" class="flex min-w-0 flex-1 items-center gap-1.5 rounded-full px-2.5 py-1.5 text-left hover:bg-accent" :aria-label="jumpLabel" :title="text" @click="emit('jump')">
      <Reply class="size-3.5 shrink-0" aria-hidden="true" /><span class="shrink-0 font-medium">{{ label }}</span><span class="truncate text-muted-foreground">{{ text }}</span>
    </button>
    <Button type="button" variant="ghost" size="icon-sm" class="size-6 shrink-0 rounded-full" :aria-label="cancelLabel" @click="emit('cancel')"><X class="size-3.5" aria-hidden="true" /></Button>
  </div>
</template>
