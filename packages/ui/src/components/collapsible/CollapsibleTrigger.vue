<script setup lang="ts">
import { definedProps } from "@ivy/ui/lib/utils"
import type { CollapsibleTriggerProps } from "reka-ui"
import { CollapsibleTrigger } from "reka-ui"

const props = defineProps<CollapsibleTriggerProps>()
</script>

<template>
  <CollapsibleTrigger
    data-slot="collapsible-trigger"
    v-bind="definedProps(props)"
  >
    <slot />
  </CollapsibleTrigger>
</template>
