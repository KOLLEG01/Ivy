<script setup lang="ts">
import type { ComboboxInputEmits, ComboboxInputProps } from "reka-ui"
import type { HTMLAttributes } from "vue"
import { reactiveOmit } from "@vueuse/core"
import { ComboboxInput, useForwardPropsEmits } from "reka-ui"
import { cn, definedProps } from "@ivy/ui/lib/utils"

defineOptions({
  inheritAttrs: false,
})

const props = defineProps<ComboboxInputProps & {
  class?: HTMLAttributes["class"]
}>()

const emits = defineEmits<ComboboxInputEmits>()

const delegatedProps = reactiveOmit(props, "class")

const forwarded = useForwardPropsEmits(delegatedProps, emits)
</script>

<template>
  <ComboboxInput
    data-slot="combobox-input"
    v-bind="definedProps({ ...$attrs, ...forwarded })"
    :class="cn('placeholder:text-muted-foreground flex h-9 w-full min-w-0 bg-transparent py-1 text-base outline-hidden disabled:cursor-not-allowed disabled:opacity-50 md:text-sm', props.class)"
  >
    <slot />
  </ComboboxInput>
</template>
