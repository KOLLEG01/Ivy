<script setup lang="ts">
import { definedProps } from "@ivy/ui/lib/utils"
import type { CollapsibleRootEmits, CollapsibleRootProps } from "reka-ui"
import { CollapsibleRoot, useForwardPropsEmits } from "reka-ui"

const props = defineProps<CollapsibleRootProps>()
const emits = defineEmits<CollapsibleRootEmits>()

const forwarded = useForwardPropsEmits(props, emits)
</script>

<template>
  <CollapsibleRoot
    v-slot="slotProps"
    data-slot="collapsible"
    v-bind="definedProps(forwarded)"
  >
    <slot v-bind="slotProps" />
  </CollapsibleRoot>
</template>
