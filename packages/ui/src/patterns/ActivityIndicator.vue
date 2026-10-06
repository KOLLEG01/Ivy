<script setup lang="ts">
import { LoaderCircle } from "@lucide/vue";
// A compact live-state mark for navigation rows: work in progress or results not yet seen.
const props = defineProps<{
  state: "working" | "unread";
  label?: string | undefined;
}>();
</script>
<template>
  <span
    role="img"
    :aria-label="
      props.label ?? (state === 'working' ? 'Working' : 'Unread results')
    "
    :title="props.label ?? (state === 'working' ? 'Working' : 'Unread results')"
    :data-state="state"
    class="flex size-4 shrink-0 items-center justify-center"
  >
    <LoaderCircle
      v-if="state === 'working'"
      class="size-3.5 animate-spin text-muted-foreground"
      aria-hidden="true"
    />
    <span v-else class="size-2 rounded-full bg-primary" aria-hidden="true" />
  </span>
</template>
