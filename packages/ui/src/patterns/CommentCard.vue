<script setup lang="ts">
import type { HTMLAttributes } from "vue";
import { cn } from "../lib/utils";

/** One bordered comment in a discussion: author line on top, content below. */
const props = defineProps<{
  author: string;
  meta?: string;
  highlight?: boolean;
  class?: HTMLAttributes["class"];
}>();
</script>
<template>
  <article
    data-slot="comment-card"
    :class="
      cn(
        'min-w-0 overflow-hidden rounded-lg border bg-card text-card-foreground',
        highlight && 'border-primary/40',
        props.class,
      )
    "
  >
    <header
      class="flex min-h-10 items-center gap-2 border-b bg-muted/40 px-3 py-2 text-xs"
    >
      <slot name="icon" />
      <span class="font-medium text-foreground">{{ author }}</span>
      <span v-if="meta" class="truncate text-muted-foreground">{{ meta }}</span>
      <span class="ml-auto flex shrink-0 items-center gap-1"
        ><slot name="actions"
      /></span>
    </header>
    <div class="min-w-0 space-y-3 p-3 text-sm"><slot /></div>
  </article>
</template>
