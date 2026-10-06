<script setup lang="ts">
import { definedProps } from "@ivy/ui/lib/utils"
import type { DialogRootEmits, DialogRootProps } from "reka-ui"
import { DialogRoot, useForwardPropsEmits } from "reka-ui"

const props = defineProps<DialogRootProps>()
const emits = defineEmits<DialogRootEmits>()

const forwarded = useForwardPropsEmits(props, emits)
</script>

<template>
  <DialogRoot
    v-slot="slotProps"
    data-slot="sheet"
    v-bind="definedProps(forwarded)"
  >
    <slot v-bind="slotProps" />
  </DialogRoot>
</template>
