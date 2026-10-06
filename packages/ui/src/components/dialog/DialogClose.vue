<script setup lang="ts">
import { definedProps } from "@ivy/ui/lib/utils"
import type { DialogCloseProps } from "reka-ui"
import { DialogClose } from "reka-ui"

const props = defineProps<DialogCloseProps>()
</script>

<template>
  <DialogClose
    data-slot="dialog-close"
    v-bind="definedProps(props)"
  >
    <slot />
  </DialogClose>
</template>
