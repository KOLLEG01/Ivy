<script setup lang="ts">
import { computed, ref } from "vue";
import { Check, Plus, X } from "@lucide/vue";
import {
  Combobox,
  ComboboxAnchor,
  ComboboxGroup,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
  ComboboxViewport,
} from "../components/combobox";
import { Badge } from "../components/badge";
import { cn } from "../lib/utils";

export interface TagOption {
  value: string;
  label: string;
  /** Secondary text shown in the list and searched with the label. */
  detail?: string;
  disabled?: boolean;
}
/** Pick several values as removable tags; options can be searched and, optionally, created. */
const model = defineModel<string[]>({ default: () => [] });
const props = defineProps<{
  id?: string;
  label: string;
  options: TagOption[];
  placeholder?: string;
  disabled?: boolean;
  /** Normalizes typed text into a new value, or returns null when it cannot be created. */
  create?: (text: string) => string | null;
  createLabel?: string;
  emptyText?: string;
  /** Limits rendered matches so large collections stay responsive. */
  limit?: number;
  class?: string;
}>();
const search = ref(""),
  open = ref(false);
const known = computed(
  () => new Map(props.options.map((option) => [option.value, option])),
);
const labelOf = (value: string) => known.value.get(value)?.label ?? value;
const matches = computed(() => {
  const query = search.value.trim().toLocaleLowerCase();
  const found = props.options.filter(
    (option) =>
      !query ||
      [option.value, option.label, option.detail ?? ""].some((value) =>
        value.toLocaleLowerCase().includes(query),
      ),
  );
  return found.slice(0, props.limit ?? 100);
});
const created = computed(() => {
  const value = props.create?.(search.value.trim()) ?? null;
  return value && !known.value.has(value) && !model.value.includes(value)
    ? value
    : null;
});
const update = (value: unknown) => {
  if (Array.isArray(value)) model.value = value.map(String);
  search.value = "";
};
const remove = (value: string) => {
  model.value = model.value.filter((item) => item !== value);
};
const backspace = () => {
  if (!search.value && model.value.length)
    model.value = model.value.slice(0, -1);
};
</script>
<template>
  <Combobox
    v-model:open="open"
    :model-value="model"
    multiple
    ignore-filter
    open-on-click
    :disabled="disabled"
    @update:model-value="update"
  >
    <ComboboxAnchor
      :class="
        cn(
          'flex min-h-9 w-full min-w-0 flex-wrap items-center gap-1.5 rounded-md border border-input bg-transparent px-2 py-1.5 shadow-xs transition-[color,box-shadow] focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/50 has-[input:disabled]:opacity-50 dark:bg-input/30',
          props.class,
        )
      "
    >
      <Badge
        v-for="value in model"
        :key="value"
        variant="secondary"
        class="max-w-full gap-1 pr-1"
        :data-value="value"
        ><span class="truncate">{{ labelOf(value) }}</span
        ><button
          v-if="!disabled"
          type="button"
          class="grid size-4 place-items-center rounded-full hover:bg-foreground/10"
          :aria-label="'Remove ' + labelOf(value)"
          @click.stop="remove(value)"
        >
          <X class="size-3" aria-hidden="true" /></button
      ></Badge>
      <ComboboxInput
        :id="id"
        v-model="search"
        :aria-label="label"
        :placeholder="model.length ? '' : placeholder"
        :disabled="disabled"
        class="h-6 min-w-24 flex-1 px-1 py-0"
        @keydown.backspace="backspace"
      />
    </ComboboxAnchor>
    <ComboboxList align="start" class="w-(--reka-combobox-trigger-width) min-w-56">
      <ComboboxViewport class="p-1">
        <ComboboxGroup v-if="matches.length" class="p-0">
          <ComboboxItem
            v-for="option in matches"
            :key="option.value"
            :value="option.value"
            :text-value="option.label"
            :disabled="!!option.disabled"
            ><span class="min-w-0 flex-1"
              ><span class="block truncate">{{ option.label }}</span
              ><span
                v-if="option.detail"
                class="block truncate text-xs text-muted-foreground"
                >{{ option.detail }}</span
              ></span
            ><Check
              v-if="model.includes(option.value)"
              class="ml-auto"
              aria-hidden="true"
          /></ComboboxItem>
        </ComboboxGroup>
        <ComboboxItem v-if="created" :value="created" :text-value="created"
          ><Plus aria-hidden="true" /><span class="min-w-0 truncate"
            >{{ createLabel ?? "Add" }} “{{ created }}”</span
          ></ComboboxItem
        >
        <p
          v-if="!matches.length && !created"
          class="px-2 py-4 text-center text-sm text-muted-foreground"
        >
          {{ emptyText ?? "No matches." }}
        </p>
      </ComboboxViewport>
    </ComboboxList>
  </Combobox>
</template>
