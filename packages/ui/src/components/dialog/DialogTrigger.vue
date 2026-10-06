<script setup lang="ts">
import { definedProps } from "@ivy/ui/lib/utils"
import type { DialogTriggerProps } from "reka-ui"
import { DialogTrigger } from "reka-ui"

const props = defineProps<DialogTriggerProps>()
</script>

<template>
  <DialogTrigger
    data-slot="dialog-trigger"
    v-bind="definedProps(props)"
  >
    <slot />
  </DialogTrigger>
</template>
