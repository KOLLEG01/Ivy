<script setup lang="ts">
import { definedProps } from "@ivy/ui/lib/utils"
import type { TooltipTriggerProps } from "reka-ui"
import { TooltipTrigger } from "reka-ui"

const props = defineProps<TooltipTriggerProps>()
</script>

<template>
  <TooltipTrigger
    data-slot="tooltip-trigger"
    v-bind="definedProps(props)"
  >
    <slot />
  </TooltipTrigger>
</template>
