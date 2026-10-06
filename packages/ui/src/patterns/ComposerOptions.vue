<script setup lang="ts">
import { SlidersHorizontal } from "@lucide/vue";
import { Button } from "../components/button";
import { Popover, PopoverContent, PopoverTrigger } from "../components/popover";

// A compact pill that opens further message settings; `summary` names the current non-default choices.
withDefaults(defineProps<{ title?: string; summary?: string }>(), {
  title: "Task settings",
  summary: "",
});
</script>

<template>
  <Popover>
    <PopoverTrigger as-child>
      <Button
        variant="ghost"
        size="sm"
        class="h-7 max-w-44 gap-1 rounded-full px-2.5 text-xs font-normal text-muted-foreground"
        :aria-label="title"
      >
        <SlidersHorizontal class="size-3.5" aria-hidden="true" />
        <span class="hidden truncate min-[375px]:inline">{{ summary || "Options" }}</span>
      </Button>
    </PopoverTrigger>
    <PopoverContent
      side="top"
      align="start"
      :side-offset="8"
      class="max-h-[min(70vh,28rem)] w-[min(20rem,calc(100vw-2rem))] overflow-y-auto"
    >
      <p class="mb-3 text-sm font-medium">{{ title }}</p>
      <div class="grid gap-4"><slot /></div>
    </PopoverContent>
  </Popover>
</template>
