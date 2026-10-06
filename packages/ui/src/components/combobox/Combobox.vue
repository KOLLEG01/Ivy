<script setup lang="ts">
import { definedProps } from "@ivy/ui/lib/utils"
import type { ComboboxRootEmits, ComboboxRootProps } from "reka-ui"
import { ComboboxRoot, useForwardPropsEmits } from "reka-ui"

const props = defineProps<ComboboxRootProps>()
const emits = defineEmits<ComboboxRootEmits>()

const forwarded = useForwardPropsEmits(props, emits)
</script>

<template>
  <ComboboxRoot
    v-slot="slotProps"
    data-slot="combobox"
    v-bind="definedProps(forwarded)"
  >
    <slot v-bind="slotProps" />
  </ComboboxRoot>
</template>
