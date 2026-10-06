<script setup lang="ts">
import type { HTMLAttributes } from "vue"
import { cn } from "@ivy/ui/lib/utils"
import { Separator } from "@ivy/ui/components/separator"

const props = defineProps<{
  class?: HTMLAttributes["class"]
}>()
</script>

<template>
  <Separator
    data-slot="sidebar-separator"
    data-sidebar="separator"
    :class="cn('bg-sidebar-border mx-2 w-auto', props.class)"
  >
    <slot />
  </Separator>
</template>
