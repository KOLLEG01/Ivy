<script setup lang="ts">
import { ref } from "vue";
import { Paperclip, Plus } from "@lucide/vue";
import { Button } from "../components/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "../components/dropdown-menu";

// The composer's "+" menu: photos and files first, then app-specific additions from the slot.
const props = defineProps<{ files?: boolean; disabled?: boolean; accept?: string }>();
const emit = defineEmits<{ files: [files: File[]] }>();
const input = ref<HTMLInputElement>();
const choose = (event: Event) => {
  const target = event.target as HTMLInputElement,
    files = Array.from(target.files ?? []);
  target.value = "";
  if (files.length) emit("files", files);
};
</script>

<template>
  <DropdownMenu>
    <DropdownMenuTrigger as-child>
      <Button
        variant="ghost"
        size="icon-sm"
        class="shrink-0 rounded-full text-muted-foreground"
        aria-label="Add to message"
        :disabled="disabled"
        ><Plus aria-hidden="true"
      /></Button>
    </DropdownMenuTrigger>
    <DropdownMenuContent side="top" align="start" class="w-56">
      <DropdownMenuItem v-if="props.files" @select="input?.click()"
        ><Paperclip aria-hidden="true" />Add photos and files</DropdownMenuItem
      >
      <slot />
    </DropdownMenuContent>
  </DropdownMenu>
  <input
    v-if="props.files"
    ref="input"
    type="file"
    multiple
    class="sr-only"
    tabindex="-1"
    aria-label="Choose photos and files"
    :accept="accept"
    @change="choose"
  />
</template>
