<script setup lang="ts">
import { computed } from "vue";
import { hostInitial, hostTone } from "../lib/host";

const props = defineProps<{ label: string; seed: string; remaining: number | null; compact?: boolean; unavailable?: boolean }>();
const description = computed(() => props.label + " · " + (props.unavailable ? "Offline" : props.remaining === null ? "Usage unavailable" : props.remaining + "% remaining"));
</script>

<template>
  <button type="button" data-slot="usage-pill" :aria-label="description" :title="description"
    class="inline-flex h-7 min-w-7 shrink-0 items-center justify-center gap-1.5 rounded-full px-1 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring"
    :class="[hostTone(seed), compact ? '' : 'px-2']">
    <span class="relative size-5 shrink-0" :class="{ 'opacity-50': unavailable }">
      <svg viewBox="0 0 24 24" class="size-full -rotate-90" fill="none" stroke="currentColor" stroke-width="2.5" aria-hidden="true">
        <circle cx="12" cy="12" r="10" opacity=".2" />
        <circle v-if="remaining !== null && !unavailable" cx="12" cy="12" r="10" pathLength="100" :stroke-dasharray="remaining + ' 100'" stroke-linecap="round" />
      </svg>
      <span class="absolute inset-0 flex items-center justify-center text-[9px] font-semibold">{{ hostInitial(label) }}</span>
    </span>
    <span v-if="!compact" class="truncate">{{ label }}</span>
    <span v-if="!compact" class="shrink-0 font-medium tabular-nums">{{ remaining === null || unavailable ? '—' : remaining + '%' }}</span>
  </button>
</template>
