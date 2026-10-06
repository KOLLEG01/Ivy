<script setup lang="ts">
import { definedProps } from "@ivy/ui/lib/utils"
import type { SelectGroupProps } from "reka-ui"
import { SelectGroup } from "reka-ui"

const props = defineProps<SelectGroupProps>()
</script>

<template>
  <SelectGroup
    data-slot="select-group"
    v-bind="definedProps(props)"
  >
    <slot />
  </SelectGroup>
</template>
