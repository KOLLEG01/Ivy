<script setup lang="ts" generic="T extends string">
import { cn } from "../lib/utils";
// A compact single-choice control styled like the shadcn tabs list.
const model = defineModel<T>({ required: true });
const props = defineProps<{
  label: string;
  options: Array<{ value: T; label: string }>;
  disabled?: boolean;
  class?: string;
}>();
const move = (event: KeyboardEvent, index: number) => {
  const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[
    event.key
  ];
  if (!step || props.disabled) return;
  event.preventDefault();
  const next =
    props.options[
      (index + step + props.options.length) % props.options.length
    ]!;
  model.value = next.value;
  const group = (event.currentTarget as HTMLElement).parentElement;
  (
    group?.children[props.options.indexOf(next)] as HTMLElement | undefined
  )?.focus();
};
</script>
<template>
  <div
    role="radiogroup"
    :aria-label="label"
    :class="
      cn(
        'inline-flex h-9 w-full items-center rounded-lg bg-muted p-[3px] text-muted-foreground',
        props.class,
      )
    "
  >
    <button
      v-for="(option, index) in options"
      :key="option.value"
      type="button"
      role="radio"
      :aria-checked="model === option.value"
      :tabindex="model === option.value ? 0 : -1"
      :disabled="disabled"
      class="inline-flex h-full flex-1 items-center justify-center rounded-md px-3 text-sm font-medium whitespace-nowrap transition-colors outline-none focus-visible:ring-3 focus-visible:ring-ring/50 disabled:pointer-events-none disabled:opacity-50 aria-checked:bg-background aria-checked:text-foreground aria-checked:shadow-sm dark:aria-checked:bg-input/30"
      @click="model = option.value"
      @keydown="move($event, index)"
    >
      {{ option.label }}
    </button>
  </div>
</template>
