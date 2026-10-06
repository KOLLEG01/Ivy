<script setup lang="ts">
import { definedProps } from "@ivy/ui/lib/utils"
import type { CollapsibleContentProps } from "reka-ui"
import { CollapsibleContent } from "reka-ui"

const props = defineProps<CollapsibleContentProps>()
</script>

<template>
  <CollapsibleContent
    data-slot="collapsible-content"
    v-bind="definedProps(props)"
  >
    <slot />
  </CollapsibleContent>
</template>
