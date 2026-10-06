<script setup lang="ts">
import { definedProps } from "@ivy/ui/lib/utils"
import type { TooltipProviderProps } from "reka-ui"
import { TooltipProvider } from "reka-ui"

const props = withDefaults(defineProps<TooltipProviderProps>(), {
  delayDuration: 0,
})
</script>

<template>
  <TooltipProvider v-bind="definedProps(props)">
    <slot />
  </TooltipProvider>
</template>
