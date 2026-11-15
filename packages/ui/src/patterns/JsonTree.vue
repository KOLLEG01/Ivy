<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { Button } from "../components/button";

defineOptions({ name: "JsonTree" });
const props = withDefaults(
  defineProps<{
    value: unknown;
    label?: string;
    depth?: number;
    openDepth?: number;
    stringPreviewLength?: number;
    showFullLabel?: string;
    showPreviewLabel?: string;
  }>(),
  {
    label: "value",
    depth: 0,
    openDepth: 1,
    stringPreviewLength: 256,
    showFullLabel: "Show full value",
    showPreviewLabel: "Show preview",
  },
);
const expanded = ref(false);
watch(
  () => props.value,
  () => {
    expanded.value = false;
  },
);
const truncated = computed(
  () =>
    typeof props.value === "string" &&
    props.value.length > Math.max(1, props.stringPreviewLength),
);
const object = computed(
  () => props.value !== null && typeof props.value === "object",
);
const array = computed(() => Array.isArray(props.value));
const entries = computed(() =>
  object.value ? Object.entries(props.value as Record<string, unknown>) : [],
);
const kind = computed(() =>
  array.value
    ? "array"
    : object.value
      ? "object"
      : props.value === null
        ? "null"
        : typeof props.value,
);
const preview = computed(() => {
  if (typeof props.value === "string")
    return (
      JSON.stringify(
        truncated.value && !expanded.value
          ? props.value.slice(0, Math.max(1, props.stringPreviewLength))
          : props.value,
      ) + (truncated.value && !expanded.value ? "…" : "")
    );
  if (props.value === null) return "null";
  if (typeof props.value === "number" || typeof props.value === "boolean")
    return String(props.value);
  return "";
});
</script>

<template>
  <details v-if="object" class="json-tree min-w-0" :open="depth < openDepth">
    <summary class="min-w-0 py-1 font-mono text-xs">
      <span class="break-all font-medium text-foreground">{{ label }}</span>
      <span class="ml-2 text-muted-foreground"
        >{{ kind }} · {{ entries.length }}
        {{ entries.length === 1 ? "field" : "fields" }}</span
      >
    </summary>
    <div class="ml-2 border-l pl-3 sm:ml-3 sm:pl-4">
      <JsonTree
        v-for="([key, item], index) in entries"
        :key="key + ':' + index"
        :value="item"
        :label="array ? `[${key}]` : key"
        :depth="depth + 1"
        :open-depth="openDepth"
        :string-preview-length="stringPreviewLength"
        :show-full-label="showFullLabel"
        :show-preview-label="showPreviewLabel"
      />
      <p
        v-if="entries.length === 0"
        class="py-1 font-mono text-xs text-muted-foreground"
      >
        empty
      </p>
    </div>
  </details>
  <div
    v-else
    class="grid min-w-0 grid-cols-[minmax(5rem,auto)_minmax(0,1fr)] gap-x-3 py-1 font-mono text-xs"
  >
    <span class="break-all font-medium text-foreground">{{ label }}</span>
    <span
      class="break-all whitespace-pre-wrap"
      :class="
        kind === 'string'
          ? 'text-emerald-700 dark:text-emerald-300'
          : kind === 'null'
            ? 'text-muted-foreground'
            : 'text-sky-700 dark:text-sky-300'
      "
      >{{ preview
      }}<Button
        v-if="truncated"
        variant="link"
        size="sm"
        class="h-auto px-1 py-0 text-xs"
        :aria-expanded="expanded"
        @click="expanded = !expanded"
        >{{ expanded ? showPreviewLabel : showFullLabel }}</Button
      ></span
    >
  </div>
</template>

<style scoped>
.json-tree > summary {
  list-style-position: outside;
}
</style>
