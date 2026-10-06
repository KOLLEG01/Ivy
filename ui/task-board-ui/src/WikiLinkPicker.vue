<script setup lang="ts">
import { onBeforeUnmount, ref, watch } from "vue";
import { ExternalLink, Link2 } from "@lucide/vue";
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  Input,
  useRemote,
} from "@ivy/ui";
import { client, wikiPageUrl } from "./runtime";

defineProps<{ disabled?: boolean }>();
const emit = defineEmits<{ select: [markdown: string] }>();
const open = ref(false),
  query = ref("");
let timer: ReturnType<typeof setTimeout> | undefined;
const results = useRemote(
  (signal) =>
    query.value.trim()
      ? client.request(
          "wiki.search",
          { text: query.value.trim(), limit: 20 },
          { signal },
        )
      : Promise.resolve(null),
  0,
  ["objects/wiki/page"],
);
watch(query, () => {
  clearTimeout(timer);
  timer = setTimeout(() => void results.refresh(), 200);
});
watch(open, (value) => {
  if (!value) query.value = "";
});
onBeforeUnmount(() => clearTimeout(timer));
const select = (id: string, name: string) => {
  emit("select", `[${name.replace(/([\\[\]])/g, "\\$1")}](${wikiPageUrl(id)})`);
  open.value = false;
};
</script>

<template>
  <Button
    type="button"
    variant="ghost"
    size="sm"
    :disabled="disabled"
    @click="open = true"
    ><Link2 aria-hidden="true" />Insert Wiki page</Button
  >
  <Dialog v-model:open="open">
    <DialogContent class="w-[calc(100vw-2rem)] sm:max-w-xl">
      <DialogHeader>
        <DialogTitle>Insert Wiki page</DialogTitle>
        <DialogDescription
          >Search for a page to link in this Markdown field.</DialogDescription
        >
      </DialogHeader>
      <Input
        v-model="query"
        aria-label="Search Wiki pages"
        placeholder="Search Wiki pages"
      />
      <p
        v-if="results.error.value"
        role="alert"
        class="text-sm text-destructive"
      >
        {{ results.error.value }}
      </p>
      <div class="max-h-72 space-y-1 overflow-y-auto" role="list">
        <div
          v-for="page in results.value.value?.items ?? []"
          :key="page.id"
          role="listitem"
          class="flex min-w-0 items-center gap-1"
        >
          <Button
            type="button"
            variant="ghost"
            class="min-w-0 flex-1 justify-start"
            @click="select(page.id, page.name)"
            ><span class="truncate">{{ page.name }}</span></Button
          >
          <Button type="button" variant="ghost" size="icon-sm" as-child
            ><a
              :href="wikiPageUrl(page.id)"
              target="_blank"
              rel="noopener noreferrer"
              :aria-label="'Open ' + page.name + ' in Wiki'"
              ><ExternalLink aria-hidden="true" /></a
          ></Button>
        </div>
      </div>
      <p
        v-if="
          query.trim() &&
          !results.loading.value &&
          !results.error.value &&
          !results.value.value?.items.length
        "
        class="text-sm text-muted-foreground"
      >
        No matching pages.
      </p>
    </DialogContent>
  </Dialog>
</template>
