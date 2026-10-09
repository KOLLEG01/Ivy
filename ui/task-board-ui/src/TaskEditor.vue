<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  Alert,
  AlertDescription,
  AlertTitle,
  Button,
  Checkbox,
  ChoiceChips,
  CreatableCombobox,
  Field,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLegend,
  FieldSet,
  Input,
  Label,
  LazyMarkdownEditor as MarkdownEditor,
  OptionSelect,
  PageHeader,
  SegmentedControl,
  TagPicker,
  RevisionDiff,
  useRemote,
} from "@ivy/ui";
import type { UploadedAttachment } from "@ivy/ui";
import { Plus } from "@lucide/vue";
import { priorityLabels, stateLabel, taskStarted } from "./board";
import { base, client, readDocument, taskRoute } from "./runtime";
import { canonical } from "../../../packages/sdk/src/client.js";
import { route } from "../../../packages/ui-client/src/runtime";
import type { Document, TaskBoard, Wire } from "./runtime";
import { useAction } from "./action";
import ActionState from "./ActionState.vue";
import TaskLocationPicker from "./TaskLocationPicker.vue";
import WikiLinkPicker from "./WikiLinkPicker.vue";
import { nativeRead } from "../../../packages/ui-client/src/native";
import {
  executionEnvironments,
  executionModels,
  executionSpeedAvailable,
} from "./execution-options";
import { useExecutionDefaults } from "./execution-defaults";
import { blockerCandidates } from "./blockers";
import { attachmentSource, attachmentUpload, checkAttachmentSize, useAttachmentImages } from "./attachment-images";
import { attachmentView } from "./attachment-view";
const props = defineProps<{
  workspace: TaskBoard.WorkspaceInfo;
  available: boolean;
  task?: Document<"task">;
}>();
const emit = defineEmits<{ saved: []; cancel: [] }>();
const defaults: TaskBoard.TaskFields = {
  title: "",
  description: "",
  acceptanceCriteria: [],
  category: null,
  control: "agent",
  priority: 2,
  executionRequirement: null,
  nativeOptions: { model: null, reasoningEffort: null, serviceTier: null },
  workspaceRequirement: { kind: "task_workspace" },
  dependencies: [],
  userContact: "ticket",
  allowParallel: false,
  nextReviewAt: null,
  dueAt: null,
};
const sameFields = (a: TaskBoard.TaskFields, b: TaskBoard.TaskFields) =>
  canonical(a as unknown as Wire.Json) === canonical(b as unknown as Wire.Json);
const form = ref<TaskBoard.TaskFields>(
  structuredClone(props.task?.value.fields ?? defaults),
);
const revision = ref(props.task?.pin.revision ?? null),
  baseFields = ref<TaskBoard.TaskFields | null>(
    props.task ? structuredClone(props.task.value.fields) : null,
  ),
  conflict = ref<Document<"task"> | null>(null),
  error = ref<string | null>(null);
const scope = props.task?.pin.objectId ?? "new",
  key =
    "ivy:task-board:draft:" +
    base.href +
    props.workspace.serviceNodeId +
    ":" +
    props.workspace.callerPrincipalId +
    ":" +
    scope;
let restoredDraft = false;
try {
  const raw = sessionStorage.getItem(key);
  if (raw) {
    const retained = JSON.parse(raw);
    if (
      retained.rootObjectId === props.workspace.rootObjectId &&
      retained.principalId === props.workspace.principalId
    ) {
      form.value = retained.fields;
      revision.value = retained.revision;
      baseFields.value = retained.baseFields ??
        (retained.revision === props.task?.pin.revision ? baseFields.value : null);
      // A retained copy of already-saved fields starts from the current save base.
      if (props.task && sameFields(retained.fields, props.task.value.fields)) {
        revision.value = props.task.pin.revision;
        baseFields.value = structuredClone(props.task.value.fields);
      }
      restoredDraft = true;
    }
  }
} catch {
  error.value = "The task draft cannot be read from this tab.";
}
const capabilities = ref<string[]>([...(form.value.requiredCapabilities ?? [])]);
// Acceptance criteria are edited as one Markdown list; every non-empty line is one criterion.
const criteria = ref(form.value.acceptanceCriteria.map((value) => "- " + value).join("\n")),
  showCriteria = ref(form.value.acceptanceCriteria.length > 0);
const descriptionInput = ref<{ insertMarkdown: (markdown: string) => void }>(),
  criteriaInput = ref<{ insertMarkdown: (markdown: string) => void }>();
const category = ref(form.value.category ?? "");
watch(criteria, (value) => {
  form.value.acceptanceCriteria = value
    .split("\n")
    .map((line) => line.replace(/^\s*(?:[-*+]|\d+[.)])\s+(?:\[[ xX]\]\s+)?/, "").replace(/\\$/, "").trim())
    .filter(Boolean);
});
const categorySuggestions = useRemote(
  async (signal) =>
    (await nativeRead(
      client,
      props.workspace.serviceNodeId,
      "task-board.categories",
      {},
      signal,
    )) as TaskBoard.CategoriesResult,
  0,
  ["objects/task-board/task"],
);
const blockers = useRemote(
  (signal) =>
    blockerCandidates(
      props.workspace.rootObjectId,
      props.task?.pin.objectId ?? null,
      signal,
    ),
  0,
  ["objects/task-board/task"],
);
const blockerOptions = computed(() =>
  (blockers.value.value ?? [])
    .filter(
      (task) =>
        task.workflowState !== "cancelled" ||
        form.value.dependencies.includes(task.id),
    )
    .map((task) => ({
      value: task.id,
      label: task.taskKey + " · " + task.title,
      detail: stateLabel(task.workflowState),
    })),
);

// Execution: every agent Task names one host; the configured default host is preselected.
const environments = useRemote(executionEnvironments, 30000, ["services/agent-manager"]);
const executionDefaults = useExecutionDefaults(
  () => props.workspace,
  () => props.task?.value,
);
const configured = executionDefaults.defaults;
watch(configured, (value) => {
  if (!value || props.task || restoredDraft) return;
  form.value.userContact = value.userContact ?? "ticket";
  form.value.allowParallel = value.allowParallel ?? false;
  restoredDraft = true;
}, { immediate: true });
const hosts = computed(() => {
  const result = new Map<
    string,
    { agents: typeof agents.value; capabilities: Set<string> }
  >();
  for (const hostId of environments.value.value?.hosts ?? [])
    result.set(hostId, { agents: [], capabilities: new Set() });
  for (const agent of agents.value) {
    const entry = result.get(agent.node.hostId) ?? {
      agents: [],
      capabilities: new Set<string>(),
    };
    entry.agents.push(agent);
    for (const value of agent.profile.capabilities)
      entry.capabilities.add(value.key);
    result.set(agent.node.hostId, entry);
  }
  return result;
});
const agents = computed(() => environments.value.value?.agents ?? []);
const defaultHost = computed(() =>
  configured.value?.executionRequirement?.kind === "host"
    ? configured.value.executionRequirement.hostId
    : null,
);
const missingOn = (hostId: string) => {
  const entry = hosts.value.get(hostId);
  return entry?.agents.length
    ? capabilities.value.filter((key) => !entry.capabilities.has(key))
    : capabilities.value;
};
const online = (hostId: string) => !!hosts.value.get(hostId)?.agents.length;
const savedHost =
  form.value.executionRequirement?.kind === "host"
    ? form.value.executionRequirement.hostId
    : "";
const host = ref(savedHost),
  hostTouched = ref(
    !!props.task &&
      JSON.stringify(form.value.executionRequirement) !==
        JSON.stringify(props.task.value.fields.executionRequirement),
  );
watch(
  [defaultHost, hosts],
  () => {
    if (host.value) return;
    host.value =
      defaultHost.value ??
      [...hosts.value.keys()].find(
        (hostId) => online(hostId) && !missingOn(hostId).length,
      ) ??
      "";
  },
  { immediate: true },
);
const chooseHost = (value: string) => {
  host.value = value;
  hostTouched.value = true;
};
const hostOptions = computed(() => {
  const ids = [...hosts.value.keys()];
  if (host.value && !ids.includes(host.value)) ids.unshift(host.value);
  return ids.map((hostId) => {
    const missing = missingOn(hostId),
      isDefault = hostId === defaultHost.value;
    const notes = [
      ...(isDefault ? ["default"] : []),
      ...(!hosts.value.has(hostId)
        ? ["unavailable"]
        : !online(hostId)
          ? ["offline"]
          : missing.length
            ? ["missing " + missing.join(", ")]
            : []),
    ];
    return {
      value: hostId,
      label: hostId + (notes.length ? " · " + notes.join(" · ") : ""),
      // Only the default host stays selectable when it cannot serve the Task, and it is shown invalid.
      disabled:
        !isDefault &&
        hostId !== host.value &&
        (!online(hostId) || missing.length > 0),
    };
  });
});
const hostProblem = computed(() => {
  if (!host.value || !environments.value.value) return "";
  if (!online(host.value)) return host.value + " is not ready right now.";
  const missing = missingOn(host.value);
  return missing.length
    ? host.value + " does not provide " + missing.join(", ") + "."
    : "";
});
const capabilityOptions = computed(() => {
  const labels = new Map<string, { label: string; hosts: string[] }>();
  for (const agent of agents.value)
    for (const value of agent.profile.capabilities) {
      const entry = labels.get(value.key) ?? { label: value.label, hosts: [] };
      if (!entry.hosts.includes(agent.node.hostId))
        entry.hosts.push(agent.node.hostId);
      labels.set(value.key, entry);
    }
  return [...labels]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([key, value]) => ({
      value: key,
      label: value.label === key ? key : value.label + " (" + key + ")",
      detail: "On " + value.hosts.sort().join(", "),
    }));
});
const capabilityKey = (text: string) => {
  const value = text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 64);
  return value || null;
};
const hostAgents = computed(
  () => (hosts.value.get(host.value)?.agents ?? []).filter((agent) =>
    !props.task?.value.primaryResourceRef || agent.node.serviceNodeId === props.task.value.primaryResourceRef.serviceNodeId,
  ),
);
const hostAgent = computed(() => hostAgents.value[0]);

// Native options show the effective value; a value equal to the default is stored as inherited.
const nativeModel = ref(form.value.nativeOptions?.model ?? ""),
  reasoningEffort = ref(form.value.nativeOptions?.reasoningEffort ?? ""),
  serviceTier = ref(form.value.nativeOptions?.serviceTier ?? "");
const nativeChoices = computed(() => executionModels(hostAgents.value));
const defaultModel = computed(
  () =>
    configured.value?.nativeOptions.model ??
    nativeChoices.value.find((value) => value.isDefault)?.model ??
    nativeChoices.value[0]?.model ??
    "",
);
const shownModel = computed(() => nativeModel.value || defaultModel.value);
const modelChoice = computed(() =>
  nativeChoices.value.find((value) => value.model === shownModel.value),
);
const nativeEfforts = computed(() => modelChoice.value?.efforts ?? []);
const defaultEffort = computed(() => {
  const configuredEffort = configured.value?.nativeOptions.reasoningEffort;
  return configuredEffort && nativeEfforts.value.includes(configuredEffort)
    ? configuredEffort
    : (modelChoice.value?.defaultEffort ?? "");
});
const shownEffort = computed(() => reasoningEffort.value || defaultEffort.value);
const defaultSpeed = computed(
  () => configured.value?.nativeOptions.serviceTier ?? "standard",
);
const shownSpeed = computed(() => serviceTier.value || defaultSpeed.value);
const serviceTierAvailable = computed(() =>
  executionSpeedAvailable(hostAgents.value, shownModel.value, shownEffort.value),
);
const chooseModel = (value: string) => {
  nativeModel.value = value === defaultModel.value ? "" : value;
};
const chooseEffort = (value: string) => {
  reasoningEffort.value = value === defaultEffort.value ? "" : value;
};
const chooseSpeed = (value: string) => {
  serviceTier.value = value === defaultSpeed.value ? "" : value;
};
watch([shownModel, host], () => {
  if (
    reasoningEffort.value &&
    !nativeEfforts.value.includes(reasoningEffort.value)
  )
    reasoningEffort.value = "";
  if (nativeChoices.value.length && !serviceTierAvailable.value && ['fast', 'flex'].includes(shownSpeed.value))
    serviceTier.value = "standard";
});
const effortOptions = computed(() =>
  nativeEfforts.value.map((value) => ({
    value,
    label: value.charAt(0).toUpperCase() + value.slice(1),
  })),
);
const speedOptions = computed(() => [
  { value: "standard", label: "Standard" },
  { value: "fast", label: "Fast", disabled: !serviceTierAvailable.value },
  { value: "flex", label: "Flexible", disabled: !serviceTierAvailable.value },
]);
const nativeOptionsInvalid = computed(
  () =>
    hostAgents.value.length > 0 &&
    ((!!nativeModel.value &&
      !nativeChoices.value.some(
        (value) => value.model === nativeModel.value,
      )) ||
      (!!reasoningEffort.value &&
        !nativeEfforts.value.includes(reasoningEffort.value)) ||
      (["fast", "flex"].includes(serviceTier.value) &&
        !serviceTierAvailable.value)),
);

const normalizedFields = (): TaskBoard.TaskFields => {
  const fields: TaskBoard.TaskFields = {
    ...form.value,
    title: form.value.title.trim(),
    category: category.value.trim() || null,
    // A saved Task keeps its original assignment until the host is changed here.
    executionRequirement:
      props.task && !hostTouched.value
        ? props.task.value.fields.executionRequirement
        : host.value
          ? { kind: "host", hostId: host.value }
          : null,
    nativeOptions: {
      model: nativeModel.value || null,
      reasoningEffort: reasoningEffort.value || null,
      serviceTier:
        serviceTier.value === "standard" ||
        serviceTier.value === "fast" ||
        serviceTier.value === "flex"
          ? serviceTier.value
          : null,
    },
  };
  if (capabilities.value.length)
    fields.requiredCapabilities = [...capabilities.value];
  else if (props.task?.value.fields.requiredCapabilities)
    fields.requiredCapabilities = [];
  else delete fields.requiredCapabilities;
  if (props.task && !props.task.value.fields.nativeOptions &&
      Object.values(fields.nativeOptions!).every(value => value === null))
    delete fields.nativeOptions;
  return fields;
};
let retainDraft = true;
const clearDraft = () => {
  retainDraft = false;
  sessionStorage.removeItem(key);
};
watch(
  [form, revision, baseFields, capabilities, host, category, nativeModel, reasoningEffort, serviceTier],
  () => {
    if (!retainDraft) return;
    try {
      sessionStorage.setItem(
        key,
        JSON.stringify({
          fields: normalizedFields(),
          revision: revision.value,
          baseFields: baseFields.value,
          rootObjectId: props.workspace.rootObjectId,
          principalId: props.workspace.principalId,
        }),
      );
    } catch {
      error.value = "The task draft cannot be retained in this tab.";
    }
  },
  { deep: true },
);
const action = useAction(() => props.workspace, "edit:" + scope);
// Files added before the Task exists are uploaded right after it is created.
const pendingFiles = new Map<string, File>();
const images = useAttachmentImages(
  () => props.task?.pin.objectId ?? null,
  () => props.task?.value.attachments ?? [],
  () => [form.value.description, criteria.value],
);
const uploadFile = async (file: File): Promise<UploadedAttachment> => {
  const attachmentId = crypto.randomUUID(),
    src = attachmentSource(attachmentId),
    image = attachmentView(file.name, file.type) === "image";
  if (props.task && revision.value !== null) {
    const outcome = await action.start(
      "Upload task attachment",
      await attachmentUpload(props.task.pin.objectId, revision.value, attachmentId, file),
    );
    if (!outcome?.task)
      throw new Error(action.error.value ?? action.saved.value?.detail ?? "The file could not be uploaded.");
    revision.value = outcome.task.revision;
  } else {
    checkAttachmentSize(file);
    pendingFiles.set(attachmentId, file);
  }
  if (image) images.remember(src, file);
  return { src, name: file.name, image };
};
const uploadPending = async (task: TaskBoard.ObjectPin, fields: TaskBoard.TaskFields) => {
  const text = JSON.stringify(fields);
  let revision = task.revision;
  for (const [attachmentId, file] of pendingFiles) {
    if (!text.includes(attachmentSource(attachmentId))) continue;
    const outcome = await action.start(
      "Upload task attachment",
      await attachmentUpload(task.objectId, revision, attachmentId, file),
    );
    if (!outcome?.task) return;
    revision = outcome.task.revision;
  }
};
const recoveredAction = () => {
  const saved = action.saved.value?.outcome?.task;
  clearDraft();
  if (!props.task && saved) {
    location.hash = taskRoute(props.workspace.serviceNodeId, saved.objectId);
  } else emit("saved");
};
const fixed = computed(() => !!props.task?.value.claim),
  // Once a Task has run, its native context lives on that host and in that project.
  placed = computed(() => !!props.task && taskStarted(props.task.value)),
  locked = computed(
    () =>
      !props.available ||
      props.workspace.role !== "user" ||
      action.locked.value ||
      !!error.value,
  );
const localDate = (value: string | null) => {
  if (!value) return "";
  const date = new Date(value);
  return new Date(date.getTime() - date.getTimezoneOffset() * 60000)
    .toISOString()
    .slice(0, 16);
};
const setDate = (key: "dueAt" | "nextReviewAt", event: Event) => {
  const value = (event.target as HTMLInputElement).value;
  form.value[key] = value ? new Date(value).toISOString() : null;
};
const save = async () => {
  if (locked.value) return;
  error.value = null;
  if (nativeOptionsInvalid.value) {
    error.value = "Choose a model, reasoning and speed the selected host supports.";
    return;
  }
  try {
    if (props.task) {
      const current = await readDocument(
        "task",
        props.task.pin.objectId,
        props.workspace.rootObjectId,
      );
      // Unchanged fields are not saved, so the worker is not told about an empty edit.
      const fields = normalizedFields();
      if (current.pin.revision !== revision.value && !sameFields(fields, current.value.fields)) {
        // Runtime updates and comments advance the Task revision without editing its fields.
        // A restored draft may need its original revision to recover the edit base.
        if (!baseFields.value && revision.value !== null)
          baseFields.value = (await readDocument("task", {
            objectId: current.pin.objectId, revision: revision.value,
          }, props.workspace.rootObjectId)).value.fields;
        if (!baseFields.value || !sameFields(baseFields.value, current.value.fields)) {
          conflict.value = current;
          return;
        }
      }
      let pin: TaskBoard.ObjectPin | null = current.pin;
      if (!sameFields(fields, current.value.fields)) {
        const outcome = await action.start("Save task changes", {
          action: "edit",
          taskId: current.pin.objectId,
          expectedRevision: current.pin.revision,
          fields,
        });
        pin = outcome?.task ?? null;
      }
      if (pin) {
        clearDraft();
        revision.value = pin.revision;
        conflict.value = null;
        emit("saved");
      }
    } else {
      const fields = normalizedFields();
      const outcome = await action.start("Create task", {
        action: "create",
        fields,
      });
      if (outcome?.task) {
        await uploadPending(outcome.task, fields);
        clearDraft();
        location.hash = taskRoute(
          props.workspace.serviceNodeId,
          outcome.task.objectId,
        );
      }
    }
  } catch (cause) {
    error.value =
      cause instanceof Error ? cause.message : "The task could not be saved.";
  }
};
const workerOptions = [
  { value: "agent" as const, label: "Agent" },
  { value: "user" as const, label: "Me" },
];
const priorityOptions = [4, 3, 2, 1, 0].map((value) => ({
  value,
  label: priorityLabels[value]!,
}));
const contactOptions = [
  { value: "ticket" as const, label: "Ticket only" },
  { value: "chat" as const, label: "Ticket and chat" },
  { value: "phone" as const, label: "Ticket and call" },
];
const setWorktree = (value: boolean | "indeterminate") => {
  if (form.value.workspaceRequirement.kind === "existing_project")
    form.value.workspaceRequirement = {
      ...form.value.workspaceRequirement,
      useWorktree: value === true,
    };
};
const cancel = () => {
  if (props.task) emit("cancel");
  else location.hash = route("tasks", { node: props.workspace.serviceNodeId });
};
</script>
<template>
  <section :class="task ? '' : 'mx-auto w-full max-w-3xl'">
    <PageHeader v-if="!task" title="New task" />
    <form class="space-y-8" @submit.prevent="save">
      <FieldGroup>
        <Field
          ><Label for="task-title">Title</Label
          ><Input
            id="task-title"
            v-model="form.title"
            required
            :maxlength="512"
            placeholder="What needs to be done?"
            class="h-10 text-base"
            :disabled="locked || fixed"
        /></Field>
        <Field
          ><Label>Description</Label
          ><MarkdownEditor
            ref="descriptionInput"
            v-model="form.description"
            variant="field"
            label="Description"
            :disabled="locked || fixed"
            :trusted-image-urls="images.urls.value"
            :trusted-attachments="images.files.value"
            :upload-file="uploadFile"
            placeholder="Describe the outcome in language the worker can act on."
          ><template #actions>
            <WikiLinkPicker :disabled="locked || fixed" @select="descriptionInput?.insertMarkdown($event)" />
          </template></MarkdownEditor></Field>
        <Field v-if="showCriteria"
          ><Label>Acceptance criteria</Label
          ><MarkdownEditor
            ref="criteriaInput"
            v-model="criteria"
            variant="field"
            label="Acceptance criteria"
            :disabled="locked || fixed"
            :trusted-image-urls="images.urls.value"
            :trusted-attachments="images.files.value"
            :upload-file="uploadFile"
            placeholder="One verifiable criterion per line"
          ><template #actions>
            <WikiLinkPicker :disabled="locked || fixed" @select="criteriaInput?.insertMarkdown($event)" />
          </template></MarkdownEditor></Field>
        <div v-else>
          <Button
            type="button"
            variant="outline"
            size="sm"
            :disabled="locked || fixed"
            @click="showCriteria = true"
            ><Plus aria-hidden="true" />Add acceptance criteria</Button
          >
        </div>
      </FieldGroup>

      <FieldSet>
        <FieldLegend>Properties</FieldLegend>
        <FieldGroup>
          <div class="grid gap-6 sm:grid-cols-2">
            <Field
              ><Label>Worker</Label
              ><SegmentedControl
                v-model="form.control"
                label="Worker"
                :options="workerOptions"
                :disabled="locked || fixed"
            /></Field>
            <Field
              ><Label for="task-category">Category</Label
              ><CreatableCombobox
                id="task-category"
                v-model="category"
                :options="categorySuggestions.value.value?.categories ?? []"
                :maxlength="128"
                placeholder="None"
                create-label="Add category"
                :disabled="locked"
            /></Field>
          </div>
          <Field
            ><Label>Priority</Label
            ><ChoiceChips
              v-model="form.priority"
              label="Priority"
              :options="priorityOptions"
              :disabled="locked"
          /></Field>
          <Field
            ><Label>Updates</Label
            ><ChoiceChips
              v-model="form.userContact"
              label="Updates"
              :options="contactOptions"
              :disabled="locked"
          /></Field>
          <div v-if="form.control === 'user'" class="grid gap-6 sm:grid-cols-2">
            <Field
              ><Label for="task-due-date">Due</Label
              ><Input
                id="task-due-date"
                type="datetime-local"
                :model-value="localDate(form.dueAt)"
                :disabled="locked"
                @change="setDate('dueAt', $event)"
            /></Field>
            <Field
              ><Label for="task-review-date">Next review</Label
              ><Input
                id="task-review-date"
                type="datetime-local"
                :model-value="localDate(form.nextReviewAt)"
                :disabled="locked"
                @change="setDate('nextReviewAt', $event)"
            /></Field>
          </div>
        </FieldGroup>
      </FieldSet>

      <FieldSet v-if="form.control === 'agent'">
        <FieldLegend>Execution</FieldLegend>
        <FieldGroup>
          <Field
            ><Label for="task-capabilities">Dependencies</Label
            ><TagPicker
              id="task-capabilities"
              v-model="capabilities"
              label="Dependencies"
              :options="capabilityOptions"
              :create="capabilityKey"
              create-label="Require"
              placeholder="None"
              empty-text="No host advertises a capability yet."
              :disabled="locked || fixed"
            /><FieldDescription
              >Capabilities the host must provide.</FieldDescription
            ></Field
          >
          <Field :data-invalid="!!hostProblem"
            ><Label for="task-host">Host</Label
            ><OptionSelect
              id="task-host"
              class="w-full"
              :model-value="host"
              :aria-invalid="!!hostProblem"
              :disabled="locked || placed || !hostOptions.length"
              :placeholder="environments.loading.value ? 'Loading hosts…' : 'No host available'"
              @update:model-value="chooseHost"
              ><option
                v-for="option in hostOptions"
                :key="option.value"
                :value="option.value"
                :disabled="option.disabled"
              >
                {{ option.label }}
              </option></OptionSelect
            ><FieldError v-if="hostProblem">{{ hostProblem }}</FieldError></Field
          >
          <Field
            ><Label for="task-location">Project</Label
            ><TaskLocationPicker
              id="task-location"
              v-model="form.workspaceRequirement"
              :host="host"
              :agent="hostAgent"
              :default-worktree="configured?.useWorktree ?? false"
              :disabled="locked || placed"
            /><Label
              v-if="form.workspaceRequirement.kind === 'existing_project'"
              for="task-use-worktree"
              class="font-normal"
              ><Checkbox
                id="task-use-worktree"
                :model-value="form.workspaceRequirement.useWorktree"
                :disabled="locked || placed"
                @update:model-value="setWorktree"
              />Use a dedicated worktree</Label
            ><Label
              v-if="(form.workspaceRequirement.kind === 'existing_project' && !form.workspaceRequirement.useWorktree) || form.workspaceRequirement.kind === 'directory_path'"
              for="task-allow-parallel"
              class="font-normal"
              ><Checkbox
                id="task-allow-parallel"
                :model-value="form.allowParallel ?? false"
                :disabled="locked || fixed"
                @update:model-value="form.allowParallel = $event === true"
              />Allow parallel tickets in this project</Label
            ><FieldDescription v-if="placed"
              >Host and project stay fixed after the first run, where the
              agent's work lives.</FieldDescription
            ></Field
          >
          <div class="grid gap-6 sm:grid-cols-2">
            <Field
              ><Label for="task-model">Model</Label
              ><OptionSelect
                id="task-model"
                class="w-full"
                :model-value="shownModel"
                :disabled="locked || fixed || !nativeChoices.length"
                :placeholder="hostAgents.length ? 'Host default' : 'Host not ready'"
                @update:model-value="chooseModel"
                ><option
                  v-if="
                    shownModel &&
                    !nativeChoices.some((choice) => choice.model === shownModel)
                  "
                  :value="shownModel"
                >
                  {{ shownModel }}
                </option>
                <option
                  v-for="choice in nativeChoices.filter((value) => !value.hidden)"
                  :key="choice.id"
                  :value="choice.model"
                >
                  {{ choice.name }}
                </option>
                <optgroup
                  v-if="nativeChoices.some((value) => value.hidden)"
                  label="More models"
                >
                  <option
                    v-for="choice in nativeChoices.filter((value) => value.hidden)"
                    :key="choice.id"
                    :value="choice.model"
                  >
                    {{ choice.name }}
                  </option>
                </optgroup></OptionSelect
              ></Field
            >
            <Field
              ><Label>Speed</Label
              ><ChoiceChips
                :model-value="shownSpeed"
                label="Speed"
                :options="speedOptions"
                :disabled="locked || fixed"
                @update:model-value="chooseSpeed"
            /></Field>
          </div>
          <Field v-if="effortOptions.length"
            ><Label>Reasoning</Label
            ><ChoiceChips
              :model-value="shownEffort"
              label="Reasoning"
              :options="effortOptions"
              :disabled="locked || fixed"
              @update:model-value="chooseEffort"
          /></Field>
          <Alert v-if="environments.error.value" variant="destructive"
            ><AlertTitle>Hosts unavailable</AlertTitle
            ><AlertDescription>{{
              environments.error.value
            }}</AlertDescription></Alert
          >
        </FieldGroup>
      </FieldSet>

      <Field
        ><Label for="task-blockers">Blockers</Label
        ><TagPicker
          id="task-blockers"
          v-model="form.dependencies"
          label="Blockers"
          :options="blockerOptions"
          placeholder="Search tasks"
          :empty-text="blockers.loading.value ? 'Loading tasks…' : 'No other task can block this one.'"
          :disabled="locked || fixed"
        /><FieldDescription
          >This task starts after these tasks are done.</FieldDescription
        ></Field
      >

      <Alert v-if="error" variant="destructive"
        ><AlertTitle>Task not saved</AlertTitle
        ><AlertDescription>{{ error }}</AlertDescription></Alert
      >
      <Alert v-if="conflict"
        ><AlertTitle>The task changed while you were editing</AlertTitle
        ><AlertDescription
          ><p>
            Your draft is retained. Compare it with the current saved fields.
          </p>
          <RevisionDiff
            class="mt-2 w-full"
            before-label="Current saved fields"
            after-label="Your draft"
            :before="JSON.stringify(conflict.value.fields, null, 2)"
            :after="JSON.stringify(normalizedFields(), null, 2)"
          /><Button
            class="mt-2"
            type="button"
            variant="outline"
            @click="
              revision = conflict.pin.revision;
              baseFields = JSON.parse(JSON.stringify(conflict.value.fields));
              conflict = null;
            "
            >Use current revision with my draft</Button
          ></AlertDescription
        ></Alert
      >
      <div
        class="sticky bottom-0 z-10 -mx-1 flex items-center justify-end gap-2 border-t bg-background px-1 py-3 pb-[max(0.75rem,env(safe-area-inset-bottom))]"
      >
        <Button type="button" variant="ghost" @click="cancel">Cancel</Button>
        <Button
          type="submit"
          :disabled="
            locked ||
            !form.title.trim() ||
            nativeOptionsInvalid ||
            !!conflict
          "
          >{{ task ? "Save changes" : "Create task" }}</Button
        >
      </div>
    </form>
    <ActionState :action="action" @changed="recoveredAction" />
  </section>
</template>
