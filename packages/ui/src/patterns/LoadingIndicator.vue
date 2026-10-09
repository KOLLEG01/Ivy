<script setup lang="ts">
import { LoaderCircle } from "@lucide/vue";
import { cn } from "../lib/utils";
// A visible progress note for content that loads into a view already showing data. `floating`
// pins it as a pill to the top of the surrounding scroll region without shifting its content.
const props = defineProps<{ label: string; floating?: boolean; class?: string }>();
</script>
<template>
  <div v-if="floating" :class="cn('pointer-events-none sticky top-2 z-10 flex h-0 justify-center overflow-visible', props.class)">
    <p role="status" class="flex h-fit items-center gap-2 rounded-full border bg-background/95 px-3 py-1.5 text-xs text-muted-foreground shadow-sm backdrop-blur">
      <LoaderCircle class="size-3.5 animate-spin" aria-hidden="true" />{{ label }}
    </p>
  </div>
  <p v-else role="status" :class="cn('flex items-center justify-center gap-2 py-2 text-xs text-muted-foreground', props.class)">
    <LoaderCircle class="size-3.5 animate-spin" aria-hidden="true" />{{ label }}
  </p>
</template>
