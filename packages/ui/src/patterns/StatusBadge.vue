<script setup lang="ts">
import { computed } from "vue";
import { Badge } from "../components/badge";
const props = defineProps<{
  label: string;
  tone?: "good" | "bad" | "warning" | "neutral";
}>();
const dot = computed(
  () =>
    ({
      good: "bg-emerald-500",
      bad: "bg-destructive",
      warning: "bg-amber-500",
      neutral: "bg-muted-foreground/50",
    })[props.tone ?? "neutral"],
);
</script>
<template>
  <Badge
    variant="outline"
    class="gap-1.5 font-normal"
    :class="tone === 'bad' ? 'text-destructive' : 'text-foreground'"
    ><span
      class="size-1.5 rounded-full"
      :class="dot"
      aria-hidden="true"
    /><span>{{ label }}</span></Badge
  >
</template>
