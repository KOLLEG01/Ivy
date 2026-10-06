<script setup lang="ts">
import { definedProps } from "@ivy/ui/lib/utils"
import type { Component } from "vue"
import type { SidebarMenuButtonProps } from "./SidebarMenuButtonChild.vue"
import { reactiveOmit } from "@vueuse/core"
import { Tooltip, TooltipContent, TooltipTrigger } from "@ivy/ui/components/tooltip"
import SidebarMenuButtonChild from "./SidebarMenuButtonChild.vue"
import { useSidebar } from "./utils"

defineOptions({
  inheritAttrs: false,
})

const props = withDefaults(defineProps<SidebarMenuButtonProps & {
  tooltip?: string | Component
}>(), {
  as: "button",
  variant: "default",
  size: "default",
})

const { isMobile, state } = useSidebar()

const delegatedProps = reactiveOmit(props, "tooltip")
</script>

<template>
  <SidebarMenuButtonChild v-if="!tooltip" v-bind="definedProps({ ...delegatedProps, ...$attrs })">
    <slot />
  </SidebarMenuButtonChild>

  <Tooltip v-else>
    <TooltipTrigger as-child>
      <SidebarMenuButtonChild v-bind="definedProps({ ...delegatedProps, ...$attrs })">
        <slot />
      </SidebarMenuButtonChild>
    </TooltipTrigger>
    <TooltipContent
      side="right"
      align="center"
      :hidden="state !== 'collapsed' || isMobile"
    >
      <template v-if="typeof tooltip === 'string'">
        {{ tooltip }}
      </template>
      <component :is="tooltip" v-else />
    </TooltipContent>
  </Tooltip>
</template>
