<script setup lang="ts">
import { computed, useAttrs } from "vue";
import { FolderGit2, FolderOpen, FolderPlus } from "@lucide/vue";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectSeparator,
  SelectTrigger,
  SelectValue,
} from "../components/select";

defineOptions({ inheritAttrs: false });
const attrs = useAttrs();
const model = defineModel<string>({ default: "" });
const props = withDefaults(
  defineProps<{
    id?: string | undefined;
    projects: Array<{ value: string; name: string; path: string }>;
    host?: string | undefined;
    selectedLabel?: string;
    emptyLabel?: string;
    emptyValue?: string;
    addLabel?: string;
    createLabel?: string;
    disabled?: boolean | undefined;
    actionsDisabled?: boolean;
  }>(),
  {
    emptyLabel: "Choose project",
    emptyValue: "",
    addLabel: "Choose existing directory…",
    createLabel: "New project…",
  },
);
const emit = defineEmits<{ add: []; create: [] }>();
const empty = "\u0000ivy-no-project";
const label = computed(
  () =>
    props.selectedLabel ||
    props.projects.find((project) => project.value === model.value)?.name ||
    props.emptyLabel,
);
const choose = (value: unknown) => {
  if (value === "@add") emit("add");
  else if (value === "@new") emit("create");
  else model.value = value === empty ? props.emptyValue : String(value);
};
</script>

<template>
  <Select
    :model-value="model === emptyValue ? empty : model"
    :disabled="disabled"
    @update:model-value="choose"
  >
    <SelectTrigger
      v-bind="attrs"
      :id="id"
      :data-value="model"
      class="max-w-full min-w-0"
    >
      <SelectValue
        ><span class="truncate">{{ label }}</span></SelectValue
      >
    </SelectTrigger>
    <SelectContent>
      <SelectItem :value="empty">{{ emptyLabel }}</SelectItem>
      <SelectItem
        v-if="
          model !== emptyValue &&
          !projects.some((project) => project.value === model)
        "
        :value="model"
      >
        <FolderOpen aria-hidden="true" /><span class="truncate">{{
          label
        }}</span>
      </SelectItem>
      <SelectGroup v-if="projects.length">
        <SelectLabel v-if="host">Projects on {{ host }}</SelectLabel>
        <SelectItem
          v-for="project in projects"
          :key="project.value"
          :value="project.value"
        >
          <FolderGit2 aria-hidden="true" />
          <span class="min-w-0"
            ><span class="block truncate">{{ project.name }}</span>
            <span class="block truncate text-xs text-muted-foreground">{{
              project.path
            }}</span>
          </span>
        </SelectItem>
      </SelectGroup>
      <SelectSeparator />
      <SelectItem value="@add" :disabled="actionsDisabled">
        <FolderOpen aria-hidden="true" /><span>{{ addLabel }}</span>
      </SelectItem>
      <SelectItem value="@new" :disabled="actionsDisabled">
        <FolderPlus aria-hidden="true" /><span>{{ createLabel }}</span>
      </SelectItem>
    </SelectContent>
  </Select>
</template>
