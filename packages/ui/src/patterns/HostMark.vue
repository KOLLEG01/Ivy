<script setup lang="ts">
import { computed } from "vue";
import { cn } from "../lib/utils";
import { hostInitial, hostTone } from "../lib/host";

/** A small icon-sized initial that tells hosts apart; the same seed always gets the same tint. */
const props = defineProps<{ label: string; seed?: string; class?: string }>();
const initial = computed(() => hostInitial(props.label));
const tone = computed(() => hostTone(props.seed ?? props.label));
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
