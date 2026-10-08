<script setup lang="ts">
import { computed } from "vue";
import { cn } from "../lib/utils";

/** A small icon-sized initial that tells hosts apart; the same seed always gets the same tint. */
const props = defineProps<{ label: string; seed?: string; class?: string }>();
const tones = [
  "bg-sky-500/15 text-sky-700 dark:text-sky-300",
  "bg-violet-500/15 text-violet-700 dark:text-violet-300",
  "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300",
  "bg-amber-500/20 text-amber-800 dark:text-amber-300",
  "bg-rose-500/15 text-rose-700 dark:text-rose-300",
  "bg-teal-500/15 text-teal-700 dark:text-teal-300",
  "bg-indigo-500/15 text-indigo-700 dark:text-indigo-300",
  "bg-orange-500/15 text-orange-700 dark:text-orange-300",
];
const initial = computed(
  () => props.label.match(/[\p{L}\p{N}]/u)?.[0]?.toLocaleUpperCase() ?? "?",
);
const tone = computed(() => {
  let hash = 0;
  for (const char of props.seed ?? props.label)
    hash = (hash * 31 + char.codePointAt(0)!) >>> 0;
  return tones[hash % tones.length];
});
</script>

<template>
  <span
    aria-hidden="true"
    data-slot="host-mark"
    :class="
      cn(
        'inline-flex size-4 shrink-0 items-center justify-center rounded-[4px] text-[10px] leading-none font-semibold',
        tone,
        props.class,
      )
    "
    >{{ initial }}</span
  >
</template>
