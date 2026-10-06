<script setup lang="ts">
import { computed } from 'vue';
const props = defineProps<{ before: string; after: string; beforeLabel: string; afterLabel: string }>();
const maximum = 262144;
const difference = computed(() => {
  const a = props.before.slice(0, maximum).split('\n'), b = props.after.slice(0, maximum).split('\n');
  let start = 0, end = 0;
  while (start < a.length && start < b.length && a[start] === b[start]) start++;
  while (end < a.length - start && end < b.length - start && a[a.length - 1 - end] === b[b.length - 1 - end]) end++;
  return { same: props.before === props.after, start, end, removed: a.slice(start, a.length - end), added: b.slice(start, b.length - end) };
});
</script>
<template>
  <div class="space-y-3"><p v-if="before.length > maximum || after.length > maximum" class="text-sm text-muted-foreground" role="status">Comparison limited to the first {{ maximum.toLocaleString() }} characters of each revision.</p>
    <p v-if="difference.same" class="rounded-lg border p-4 text-sm">These revisions have identical content.</p>
    <template v-else><p class="text-xs text-muted-foreground">{{ difference.start }} equal leading lines · {{ difference.end }} equal trailing lines. The changed span is shown in full.</p><div class="grid min-w-0 gap-3 lg:grid-cols-2"><section class="min-w-0 rounded-lg border"><h3 class="border-b px-3 py-2 text-sm font-medium">{{ beforeLabel }} · Removed span</h3><pre class="max-h-96 overflow-auto whitespace-pre-wrap break-words bg-muted/60 p-3 font-mono text-xs leading-relaxed">{{ difference.removed.join('\n') || '(empty)' }}</pre></section><section class="min-w-0 rounded-lg border"><h3 class="border-b px-3 py-2 text-sm font-medium">{{ afterLabel }} · Added span</h3><pre class="max-h-96 overflow-auto whitespace-pre-wrap break-words bg-muted/60 p-3 font-mono text-xs leading-relaxed">{{ difference.added.join('\n') || '(empty)' }}</pre></section></div></template>
  </div>
</template>
