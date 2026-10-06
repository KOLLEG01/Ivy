<script setup lang="ts">
// Places page-level titles and actions into the shared shell top bar.
withDefaults(defineProps<{ side?: "start" | "end"; inline?: boolean }>(), {
  side: "start",
  inline: false,
});
</script>

<template>
  <Teleport defer :to="'#ivy-toolbar-' + side" :disabled="inline">
    <slot />
  </Teleport>
</template>
