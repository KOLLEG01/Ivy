<script setup lang="ts">
import { useSidebar } from "../components/sidebar";
import { Tooltip, TooltipContent, TooltipTrigger } from "../components/tooltip";
// App rail labels help pointer users; the mobile sheet shows no hover state and must close on Escape.
defineProps<{ label: string }>();
const { isMobile } = useSidebar();
</script>
<template>
  <Tooltip :disabled="isMobile" ignore-non-keyboard-focus>
    <TooltipTrigger as-child><slot /></TooltipTrigger>
    <TooltipContent side="right">{{ label }}</TooltipContent>
  </Tooltip>
</template>
