<script setup lang="ts">
import type { HTMLAttributes } from "vue";
import { ChevronRight } from "@lucide/vue";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "../components/collapsible";
import { cn } from "../lib/utils";

// One disclosure treatment for optional detail: a chevron trigger over lazily rendered content.
const open = defineModel<boolean>("open", { default: false });
const props = defineProps<{
  title?: string;
  defaultOpen?: boolean;
  variant?: "inline" | "card";
  class?: HTMLAttributes["class"];
  triggerClass?: HTMLAttributes["class"];
}>();
if (props.defaultOpen) open.value = true;
</script>

<template>
  <Collapsible
    v-model:open="open"
    data-slot="disclosure"
    :class="
      cn(
        variant === 'card' && 'rounded-lg border bg-card p-4',
        'min-w-0',
        props.class,
      )
    "
  >
    <CollapsibleTrigger
      :class="
        cn(
          'group flex w-full min-w-0 items-center gap-1.5 rounded-sm text-left text-sm text-muted-foreground outline-none hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50 data-[state=open]:text-foreground',
          variant === 'card' && 'font-medium text-foreground',
          triggerClass,
        )
      "
    >
      <ChevronRight
        class="size-4 shrink-0 transition-transform group-data-[state=open]:rotate-90"
        aria-hidden="true"
      />
      <slot name="trigger">{{ title }}</slot>
    </CollapsibleTrigger>
    <CollapsibleContent class="mt-3 min-w-0"><slot /></CollapsibleContent>
  </Collapsible>
</template>
