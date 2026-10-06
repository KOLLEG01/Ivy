<script setup lang="ts" generic="T extends string | number">
import { ToggleGroup, ToggleGroupItem } from "../components/toggle-group";
import { cn } from "../lib/utils";

/** A wrapped row of chips for picking exactly one value from a short, fixed list. */
const model = defineModel<T>({ required: true });
const props = defineProps<{
  label: string;
  options: Array<{ value: T; label: string; description?: string; disabled?: boolean }>;
  disabled?: boolean;
  class?: string;
}>();
// A single toggle group clears its value when the pressed chip is clicked again; one choice stays required.
const choose = (value: unknown) => {
  if (value !== undefined && value !== null && value !== "") model.value = value as T;
};
</script>
<template>
  <ToggleGroup
    type="single"
    variant="outline"
    size="sm"
    :spacing="2"
    :model-value="model"
    :aria-label="label"
    :disabled="disabled"
    :class="cn('w-full flex-wrap', props.class)"
    @update:model-value="choose"
  >
    <ToggleGroupItem
      v-for="option in options"
      :key="option.value"
      :value="option.value"
      :disabled="!!option.disabled"
      :title="option.description"
      class="rounded-full data-[state=on]:border-primary data-[state=on]:bg-primary data-[state=on]:text-primary-foreground"
      >{{ option.label }}</ToggleGroupItem
    >
  </ToggleGroup>
</template>
