<script setup lang="ts">
import { computed } from "vue";
import { useEventListener } from "@vueuse/core";
import { Button } from "../components/button";
import { useSidebar } from "../components/sidebar";

const { isMobile, open, openMobile, setOpenMobile, toggleSidebar } =
  useSidebar();
// Hash navigation inside the mobile sheet replaces the page, so the sheet closes with it.
useEventListener(window, "hashchange", () => setOpenMobile(false));
const expanded = computed(() =>
  isMobile.value ? openMobile.value : open.value,
);
</script>

<template>
  <Button
    variant="ghost"
    size="icon-sm"
    aria-label="Toggle navigation"
    aria-controls="ivy-navigation"
    :aria-expanded="expanded"
    @click="toggleSidebar"
  >
    <slot />
  </Button>
</template>
