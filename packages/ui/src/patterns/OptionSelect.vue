<script setup lang="ts">
import { Fragment, useAttrs, useSlots } from "vue";
import type { HTMLAttributes, VNode } from "vue";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "../components/select";
import { cn, definedProps } from "../lib/utils";

/**
 * The shadcn Select with native-select markup: apps keep writing `<option>` and `<optgroup>`
 * children while the open list uses the shared popover, not the browser's own menu.
 */
defineOptions({ inheritAttrs: false });
const model = defineModel<string>({ default: "" });
const props = defineProps<{
  id?: string;
  disabled?: boolean;
  required?: boolean;
  name?: string;
  placeholder?: string;
  class?: HTMLAttributes["class"];
}>();
const attrs = useAttrs(),
  slots = useSlots();
interface Choice {
  value: string;
  label: string;
  disabled: boolean;
  group: string;
}
// Select items cannot carry an empty value; an empty option keeps its meaning through this key.
const empty = "\u0000ivy-empty";
const toItem = (value: string) => (value === "" ? empty : value);
const fromItem = (value: unknown) =>
  value === empty ? "" : String(value ?? "");
const textOf = (children: unknown): string =>
  typeof children === "string"
    ? children
    : Array.isArray(children)
      ? children
          .map((child) => textOf((child as VNode)?.children ?? child))
          .join("")
      : children && typeof children === "object" && "default" in children
        ? textOf((children as { default: () => unknown }).default())
        : "";
const isOption = (node: VNode) =>
  node.type === "option" ||
  (typeof node.type === "object" &&
    (node.type as { __name?: string }).__name === "NativeSelectOption");
const isGroup = (node: VNode) =>
  node.type === "optgroup" ||
  (typeof node.type === "object" &&
    (node.type as { __name?: string }).__name === "NativeSelectOptGroup");
const read = (nodes: unknown, group = ""): Choice[] =>
  (Array.isArray(nodes) ? nodes : []).flatMap((node: VNode) => {
    if (!node || typeof node !== "object") return [];
    if (node.type === Fragment) return read(node.children, group);
    if (isGroup(node)) {
      const children = node.children as unknown;
      return read(
        children && typeof children === "object" && "default" in children
          ? (children as { default: () => unknown }).default()
          : children,
        String(node.props?.label ?? ""),
      );
    }
    if (!isOption(node)) return [];
    const label = textOf(node.children).replace(/\s+/g, " ").trim();
    const disabled = node.props?.disabled;
    return [
      {
        value: String(node.props?.value ?? label),
        label,
        disabled: disabled !== undefined && disabled !== false,
        group,
      },
    ];
  });
// Read during render so option lists stay reactive like native children.
const choices = () => read(slots.default?.());
const groups = () => {
  const result: Array<{ label: string; items: Choice[] }> = [];
  for (const choice of choices())
    if (result.at(-1)?.label === choice.group)
      result.at(-1)!.items.push(choice);
    else result.push({ label: choice.group, items: [choice] });
  return result;
};
const selectedLabel = () =>
  choices().find((choice) => choice.value === model.value)?.label ?? "";
</script>

<template>
  <Select
    v-bind="definedProps({ disabled, required, name })"
    :model-value="toItem(model)"
    @update:model-value="model = fromItem($event)"
  >
    <SelectTrigger
      :id="id"
      :data-value="model"
      v-bind="attrs"
      :class="cn('max-w-full min-w-0', props.class)"
      ><SelectValue v-bind="definedProps({ placeholder })"
        ><span class="truncate">{{
          selectedLabel() || placeholder
        }}</span></SelectValue
      ></SelectTrigger
    >
    <SelectContent>
      <template v-for="group in groups()" :key="group.label">
        <SelectGroup>
          <SelectLabel v-if="group.label">{{ group.label }}</SelectLabel>
          <SelectItem
            v-for="choice in group.items"
            :key="choice.value"
            :value="toItem(choice.value)"
            :disabled="choice.disabled"
            ><span :data-value="choice.value">{{
              choice.label
            }}</span></SelectItem
          >
        </SelectGroup>
      </template>
    </SelectContent>
  </Select>
</template>
