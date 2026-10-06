<script setup lang="ts">
import { definedProps } from "@ivy/ui/lib/utils"
import type { SelectItemTextProps } from "reka-ui"
import { SelectItemText } from "reka-ui"

const props = defineProps<SelectItemTextProps>()
</script>

<template>
  <SelectItemText
    data-slot="select-item-text"
    v-bind="definedProps(props)"
  >
    <slot />
  </SelectItemText>
</template>
