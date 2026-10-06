<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { Check, ChevronsUpDown, Plus, X } from "@lucide/vue";
import {
  Combobox,
  ComboboxAnchor,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
  ComboboxTrigger,
  ComboboxViewport,
} from "../components/combobox";
import { cn } from "../lib/utils";

/**
 * A text value with suggestions, like issue labels: pick an existing value or type a new one.
 * The typed text is the value, so leaving the field keeps it even without choosing an entry.
 */
const model = defineModel<string>({ default: "" });
const props = defineProps<{
  id?: string;
  options: string[];
  placeholder?: string;
  disabled?: boolean;
  maxlength?: number;
  createLabel?: string;
  class?: string;
}>();
const text = ref(model.value),
  open = ref(false);
watch(model, (value) => {
  if (value !== text.value.trim()) text.value = value;
});
const typed = computed(() => text.value.trim());
const matches = computed(() => {
  const query = typed.value.toLocaleLowerCase();
  return [...new Set(props.options)].filter((value) =>
    value.toLocaleLowerCase().includes(query),
  );
});
const creatable = computed(
  () => !!typed.value && !props.options.includes(typed.value),
);
const choose = (value: unknown) => {
  if (typeof value !== "string") return;
  text.value = value;
  model.value = value;
  open.value = false;
};
const commit = () => {
  if (model.value !== typed.value) model.value = typed.value;
};
const clear = () => choose("");
</script>
<template>
  <Combobox
    v-model:open="open"
    :model-value="model"
    ignore-filter
    open-on-click
    :reset-search-term-on-blur="false"
    :reset-search-term-on-select="false"
    :disabled="disabled"
    @update:model-value="choose"
  >
    <ComboboxAnchor
      :class="
        cn(
          'flex h-9 w-full min-w-0 items-center gap-1 rounded-md border border-input bg-transparent pr-1 pl-3 shadow-xs transition-[color,box-shadow] focus-within:border-ring focus-within:ring-3 focus-within:ring-ring/50 has-[input:disabled]:opacity-50 dark:bg-input/30',
          props.class,
        )
      "
    >
      <ComboboxInput
        :id="id"
        v-model="text"
        :placeholder="placeholder"
        :maxlength="maxlength"
        :disabled="disabled"
        :display-value="(value: string) => value ?? ''"
        @blur="commit"
        @keydown.enter="commit"
      />
      <button
        v-if="text && !disabled"
        type="button"
        class="grid size-6 shrink-0 place-items-center rounded-sm text-muted-foreground hover:text-foreground"
        aria-label="Clear"
        @click="clear"
      >
        <X class="size-3.5" aria-hidden="true" />
      </button>
      <ComboboxTrigger
        class="grid size-6 shrink-0 place-items-center rounded-sm text-muted-foreground hover:text-foreground"
        aria-label="Show suggestions"
      >
        <ChevronsUpDown class="size-4" aria-hidden="true" />
      </ComboboxTrigger>
    </ComboboxAnchor>
    <ComboboxList
      align="start"
      class="w-(--reka-combobox-trigger-width) min-w-48"
    >
      <ComboboxViewport class="p-1">
        <ComboboxItem
          v-for="value in matches"
          :key="value"
          :value="value"
          :text-value="value"
          ><span class="min-w-0 flex-1 truncate">{{ value }}</span
          ><Check
            v-if="value === model"
            class="ml-auto"
            aria-hidden="true"
        /></ComboboxItem>
        <ComboboxItem v-if="creatable" :value="typed" :text-value="typed"
          ><Plus aria-hidden="true" /><span class="min-w-0 truncate"
            >{{ createLabel ?? "Create" }} “{{ typed }}”</span
          ></ComboboxItem
        >
        <p
          v-if="!matches.length && !creatable"
          class="px-2 py-4 text-center text-sm text-muted-foreground"
        >
          Type to add a value.
        </p>
      </ComboboxViewport>
    </ComboboxList>
  </Combobox>
</template>
