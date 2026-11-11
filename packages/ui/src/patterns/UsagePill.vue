<script setup lang="ts">
import { computed } from "vue";
import { hostInitial, hostTone } from "../lib/host";
import { meterText, usageTone } from "../lib/usage";
import type { UsageMeterGroup, UsagePillLevel } from "../lib/usage";

// Full shows captions, meters shows unlabelled micro bars in the same order, mark only a ring.
const props = withDefaults(defineProps<{ label: string; seed: string; groups: UsageMeterGroup[]; level?: UsagePillLevel | undefined; unavailable?: boolean; status?: string }>(), { level: "full" });
const meters = computed(() => props.groups.flatMap(group => group.meters));
const highest = computed(() => {
  const known = meters.value.map(meter => meter.used).filter((used): used is number => used !== null);
  return known.length && !props.unavailable ? Math.max(...known) : null;
});
// Screen readers and hover get the same values the bars show, plus their state.
const description = computed(() => [props.label + (props.unavailable ? " · Offline" : props.status ? " · " + props.status : ""),
  ...(props.unavailable ? [] : meters.value.map(meterText))].join("\n"));
const fill = (used: number) => Math.max(used, used > 0 ? 8 : 0) + "%";
</script>

<template>
  <button type="button" data-slot="usage-pill" :aria-label="description" :title="description" :data-level="level"
    class="inline-flex h-8 min-w-7 shrink-0 items-center gap-1.5 rounded-full px-1 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring"
    :class="[hostTone(seed), level === 'mark' ? 'justify-center' : 'pr-2.5']">
    <span class="relative size-5 shrink-0" :class="{ 'opacity-50': unavailable }">
      <svg viewBox="0 0 24 24" class="size-full -rotate-90" fill="none" stroke="currentColor" stroke-width="2.5" aria-hidden="true">
        <circle cx="12" cy="12" r="10" opacity=".2" />
        <circle v-if="level === 'mark' && highest !== null" cx="12" cy="12" r="10" pathLength="100" :stroke-dasharray="highest + ' 100'"
          stroke-linecap="round" :class="{ 'text-destructive': highest >= 90, 'text-amber-500': highest >= 75 && highest < 90 }" />
      </svg>
      <span class="absolute inset-0 flex items-center justify-center text-[9px] font-semibold">{{ hostInitial(label) }}</span>
    </span>
    <span v-if="level === 'full'" class="max-w-20 truncate font-medium">{{ label }}</span>
    <span v-if="level !== 'mark' && unavailable" class="text-[10px] opacity-80">Offline</span>
    <span v-else-if="level !== 'mark'" class="flex gap-2 text-foreground" :class="level === 'full' ? 'items-end' : 'items-center'" aria-hidden="true">
      <span v-for="group in groups" :key="group.id" data-slot="usage-meter-group" class="flex flex-col gap-0.5">
        <span v-if="level === 'full' && group.caption" class="max-w-16 truncate text-[9px] leading-none font-medium opacity-70">{{ group.caption }}</span>
        <span class="flex items-end" :class="level === 'full' ? 'gap-1' : 'gap-0.5'">
          <span v-for="meter in group.meters" :key="meter.id" class="flex flex-col items-center gap-0.5" :title="meterText(meter)">
            <template v-if="level === 'full'">
              <span v-if="!group.caption" class="text-[9px] leading-none font-medium opacity-70">{{ meter.caption }}</span>
              <span data-slot="usage-meter" class="relative h-1.5 w-5 overflow-hidden rounded-full"
                :class="meter.used === null ? 'border border-dashed border-foreground/30' : 'bg-foreground/15'">
                <span v-if="meter.used !== null" class="absolute inset-y-0 left-0 rounded-full" :class="usageTone(meter.used)" :style="{ width: fill(meter.used) }" />
              </span>
            </template>
            <span v-else data-slot="usage-meter" class="relative h-4 w-1 overflow-hidden rounded-full"
              :class="meter.used === null ? 'border border-dashed border-foreground/30' : 'bg-foreground/15'">
              <span v-if="meter.used !== null" class="absolute inset-x-0 bottom-0 rounded-full" :class="usageTone(meter.used)" :style="{ height: fill(meter.used) }" />
            </span>
          </span>
        </span>
      </span>
    </span>
  </button>
</template>
