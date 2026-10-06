<script setup lang="ts">
import { definedProps } from "@ivy/ui/lib/utils"
import type { PopoverAnchorProps } from "reka-ui"
import { PopoverAnchor } from "reka-ui"

const props = defineProps<PopoverAnchorProps>()
</script>

<template>
  <PopoverAnchor
    data-slot="popover-anchor"
    v-bind="definedProps(props)"
  >
    <slot />
  </PopoverAnchor>
</template>
