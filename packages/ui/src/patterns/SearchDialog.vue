<script setup lang="ts">
import { onBeforeUnmount, onMounted } from "vue";
import { Search } from "@lucide/vue";
import { Dialog, DialogContent, DialogTitle } from "../components/dialog";
import { Kbd } from "../components/kbd";
// A command-palette style search shared by every UI; Ctrl/Cmd+K opens it.
const open = defineModel<boolean>("open", { required: true });
const query = defineModel<string>("query", { required: true });
const props = withDefaults(
  defineProps<{
    title?: string;
    label?: string;
    placeholder?: string;
    resultsLabel?: string;
    loading?: boolean;
    empty?: boolean;
    emptyText?: string;
    shortcut?: boolean;
  }>(),
  {
    title: "Search",
    label: "Search",
    placeholder: "Search…",
    resultsLabel: "Search results",
    emptyText: "No results",
    shortcut: true,
  },
);
const onKey = (event: KeyboardEvent) => {
  if (
    props.shortcut &&
    (event.ctrlKey || event.metaKey) &&
    event.key.toLowerCase() === "k"
  ) {
    event.preventDefault();
    open.value = true;
  }
};
onMounted(() => window.addEventListener("keydown", onKey));
onBeforeUnmount(() => window.removeEventListener("keydown", onKey));
</script>
<template>
  <Dialog v-model:open="open">
    <DialogContent
      class="top-[15%] max-w-xl translate-y-0 gap-0 overflow-hidden p-0"
      :show-close-button="false"
    >
      <DialogTitle class="sr-only">{{ title }}</DialogTitle>
      <div class="flex items-center gap-2 border-b px-3">
        <Search
          class="size-4 shrink-0 text-muted-foreground"
          aria-hidden="true"
        />
        <input
          v-model="query"
          :aria-label="label"
          :placeholder="placeholder"
          class="h-12 min-w-0 flex-1 bg-transparent text-sm outline-hidden placeholder:text-muted-foreground"
        />
        <Kbd>Esc</Kbd>
      </div>
      <nav
        :aria-label="resultsLabel"
        class="max-h-[min(60vh,28rem)] overflow-y-auto p-2"
        @click="
          ($event.target as HTMLElement).closest('a[href]') && (open = false)
        "
      >
        <slot />
        <p
          v-if="loading"
          class="px-3 py-6 text-center text-sm text-muted-foreground"
          role="status"
        >
          Searching…
        </p>
        <p
          v-else-if="empty"
          class="px-3 py-6 text-center text-sm text-muted-foreground"
        >
          {{ emptyText }}
        </p>
      </nav>
    </DialogContent>
  </Dialog>
</template>
