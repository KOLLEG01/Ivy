<script setup lang="ts">
import { ArrowRight } from "@lucide/vue";
import Disclosure from "./Disclosure.vue";
import RevisionDiff from "./RevisionDiff.vue";

export interface FieldChange {
  field: string;
  before: string;
  after: string;
  /** Long text shows a line comparison on demand instead of both values inline. */
  long?: boolean;
}
export interface ChangeEntry {
  id: string;
  actor: string;
  at: string;
  summary?: string;
  changes: FieldChange[];
}
/** A revision history that states which fields changed and how, newest first. */
defineProps<{ entries: ChangeEntry[] }>();
const time = (value: string) => new Date(value).toLocaleString();
</script>
<template>
  <ol class="relative space-y-5 border-l pl-5" data-slot="change-history">
    <li v-for="entry in entries" :key="entry.id" class="min-w-0 text-sm">
      <span
        class="absolute -left-[5px] mt-1.5 size-2.5 rounded-full border-2 border-background bg-muted-foreground/60"
        aria-hidden="true"
      />
      <p class="text-xs text-muted-foreground">
        <span class="font-medium text-foreground">{{ entry.actor }}</span>
        {{ entry.summary ?? (entry.changes.length === 1 ? "changed 1 field" : "changed " + entry.changes.length + " fields") }}
        · <time :datetime="entry.at">{{ time(entry.at) }}</time>
      </p>
      <dl v-if="entry.changes.length" class="mt-2 space-y-1.5">
        <div
          v-for="change in entry.changes"
          :key="change.field"
          class="grid min-w-0 gap-x-3 gap-y-0.5 sm:grid-cols-[8rem_minmax(0,1fr)]"
        >
          <dt class="text-muted-foreground">{{ change.field }}</dt>
          <dd class="min-w-0">
            <Disclosure v-if="change.long" title="Show changes"
              ><RevisionDiff
                class="mt-2"
                :before="change.before"
                :after="change.after"
                before-label="Before"
                after-label="After"
            /></Disclosure>
            <span v-else class="flex min-w-0 flex-wrap items-center gap-1.5"
              ><span class="break-words text-muted-foreground line-through decoration-muted-foreground/60">{{
                change.before || "None"
              }}</span
              ><ArrowRight class="size-3.5 shrink-0 text-muted-foreground" aria-label="to" /><span
                class="break-words"
                >{{ change.after || "None" }}</span
              ></span
            >
          </dd>
        </div>
      </dl>
    </li>
  </ol>
</template>
