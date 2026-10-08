<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  Button,
  ComposerAddMenu,
  ComposerAttachments,
  ConversationComposer,
  Label,
  OptionSelect,
  ProjectSelect,
  RemoteState,
  StatusBadge,
  Textarea,
  useRemote,
  submitOnEnter,
} from "@ivy/ui";
import { ArrowUp } from "@lucide/vue";
import type { Agent } from "../../../packages/sdk/src/client.js";
import { route } from "../../../packages/ui-client/src/runtime";
import {
  nativeRead,
  optionalTool,
  permissionProfileLabel,
  permissionProfilesFrom,
  record,
  supportsFields,
  text,
} from "../../../packages/ui-client/src/native";
import { useNativeAction } from "../../../packages/ui-client/src/native-action";
import { readNativeThreadControl } from "../../../packages/ui-client/src/native-thread-control";
import { useNativeSettings, effectiveNativeSettings, nativeConfiguredHome } from '../../../packages/ui-client/src/native-settings';
import { nativeModesFrom, nativeModePayload } from '../../../packages/ui-client/src/native-modes';
import NativeActionState from "../../../packages/ui-client/src/NativeActionState.vue";
import { base, client, tr } from "./runtime";
import ProjectLocationDialog from "../../../packages/ui-client/src/ProjectLocationDialog.vue";
import { serviceTools } from "../../../packages/sdk/src/client.js";
import { normalizedPath, projectChoices } from "./task-groups";
import {
  messageInput,
  supportsInputType,
  useMessageAttachments,
} from "../../../packages/ui-client/src/message-attachments";
import type { StagedAttachment } from "../../../packages/ui-client/src/message-attachments";
const props = defineProps<{
  node: string;
  project: string;
  projectId: string;
}>();
const status = useRemote(
  async (signal) =>
    (await nativeRead(
      client,
      props.node,
      "agent.status",
      {},
      signal,
    )) as Agent.Status,
  15000,
);
const projects = useRemote(
  async (signal) =>
    (await nativeRead(
      client,
      props.node,
      "agent.projects",
      {},
      signal,
    )) as Agent.ProjectsResult,
);
const startTool = useRemote(() =>
  optionalTool(client, props.node, "codex.thread/start"),
  0, ['services']);
const sendTool = useRemote(() => optionalTool(client, props.node, 'codex.turn/start'), 0, ['services']);
const model = ref(''),
  mode = ref(''),
  effort = ref(""),
  permission = ref(""),
  cwd = ref(props.project),
  selectedProjectId = ref(props.projectId),
  projectlessKey = ref(""),
  creating = ref(false),
  continuing = ref(false),
  message = ref("");
const settings = useNativeSettings(client, () => props.node, () => cwd.value || nativeConfiguredHome(status.value.value));
const { models, permissions, modes, choices } = settings;
const profile = computed(() => settings.defaults.value.value?.document[status.value.value?.serverType ?? 'codex']);
const defaults = computed(() => effectiveNativeSettings(models.value.value, settings.config.value.value, null, profile.value));
const selectedModel = computed(() => model.value || defaults.value.model);
const modelChoice = computed(() => choices.value.find(value => value.model === selectedModel.value));
const efforts = computed(() => modelChoice.value?.efforts ?? []);
const selectedEffort = computed(() => effort.value || (model.value && model.value !== defaults.value.model ? modelChoice.value?.defaultEffort ?? '' : defaults.value.effort));
const selectedPermission = computed(() => permission.value || defaults.value.permission);
const modeChoices = computed(() => nativeModesFrom(sendTool.value.value, modes.value.value));
const selectedMode = computed(() => mode.value || defaults.value.mode);
const modePayload = computed(() => nativeModePayload(sendTool.value.value, modeChoices.value.find(value => value.mode === selectedMode.value), modelChoice.value, selectedEffort.value));
const permissionChoices = computed(() =>
  permissionProfilesFrom(permissions.value.value).filter(
    (profile) => profile.allowed,
  ),
);
const action = useNativeAction(
  client,
  "ivy:agent-create:" + base.href + props.node,
);
const firstMessage = useNativeAction(
  client,
  "ivy:agent-first-message:" + base.href + props.node,
);
const sending = computed(() => creating.value || continuing.value || action.pending.value || firstMessage.pending.value);
const attachments = useMessageAttachments(client, () => props.node);
const launchKey = "ivy:agent-launch:" + base.href + props.node;
const launch = ref<{
  message: string;
  effort: string;
  collaborationMode?: import('../../../packages/sdk/src/client.js').Wire.Json;
  attachments: StagedAttachment[];
  operationId: string;
} | null>(null);
try {
  const saved = JSON.parse(sessionStorage.getItem(launchKey) ?? "null");
  if (
    saved &&
    typeof saved.message === "string" &&
    typeof saved.operationId === "string"
  )
    launch.value = {
      message: saved.message,
      effort: text(saved.effort),
      ...(saved.collaborationMode ? { collaborationMode: saved.collaborationMode } : {}),
      attachments: Array.isArray(saved.attachments) ? saved.attachments : [],
      operationId: saved.operationId,
    };
} catch {
  action.error.value = "The first message could not be restored.";
}
const persistLaunch = () => {
  try {
    if (launch.value)
      sessionStorage.setItem(launchKey, JSON.stringify(launch.value));
    else sessionStorage.removeItem(launchKey);
  } catch {
    action.error.value =
      "The first message could not be retained. Keep this page open.";
  }
};
watch(
  () => action.saved.value?.operationId,
  (id) => {
    if (launch.value && !launch.value.operationId && id) {
      launch.value.operationId = id;
      persistLaunch();
    }
  },
  { flush: "sync" },
);
const draftKey = "ivy:agent-create-draft:" + base.href + props.node;
try {
  const draft = JSON.parse(sessionStorage.getItem(draftKey) ?? "{}");
  model.value = text(draft.model);
  mode.value = text(draft.mode);
  effort.value = text(draft.effort);
  permission.value = text(draft.permission);
  projectlessKey.value = text(draft.projectlessKey);
  message.value = text(draft.message);
  if (Array.isArray(draft.attachments)) attachments.restore(draft.attachments);
  if (!props.project) {
    cwd.value = text(draft.cwd);
    selectedProjectId.value = text(draft.projectId);
  }
} catch {
  /* New explicit selection remains possible. */
}
watch(model, () => {
  if (effort.value && !efforts.value.includes(effort.value)) effort.value = "";
});
const persistDraft = () => {
  try {
    sessionStorage.setItem(
      draftKey,
      JSON.stringify({
        model: model.value,
        mode: mode.value,
        effort: effort.value,
        permission: permission.value,
        cwd: cwd.value,
        projectId: selectedProjectId.value,
        projectlessKey: projectlessKey.value,
        message: message.value,
        attachments: attachments.staged.value,
      }),
    );
  } catch {
    action.error.value = "The new task draft cannot be retained in this tab.";
  }
};
watch(
  [
    model,
    mode,
    effort,
    permission,
    cwd,
    selectedProjectId,
    projectlessKey,
    message,
    attachments.staged,
  ],
  persistDraft,
);
const paths = computed(() =>
  projectChoices(projects.value.value?.projects ?? []),
);
const projectOptions = computed(() =>
  paths.value.map((path) => ({
    value: path.path,
    name:
      projects.value.value?.projects.find(
        (project) => project.nativeId === path.projectId,
      )?.name ?? path.label,
    path: path.path,
  })),
);
watch(
  paths,
  (values) => {
    if (cwd.value && !selectedProjectId.value)
      selectedProjectId.value =
        values.find(
          (value) => normalizedPath(value.path) === normalizedPath(cwd.value),
        )?.projectId ?? "";
  },
  { immediate: true },
);
watch(cwd, () => void permissions.refresh());
const syncProjectId = (path = cwd.value) => {
  selectedProjectId.value =
    paths.value.find(
      (value) => normalizedPath(value.path) === normalizedPath(path),
    )?.projectId ?? "";
};
const canCreate = computed(
  () =>
    !status.error.value &&
    status.value.value?.state === "ready" &&
    !action.locked.value &&
    !creating.value &&
    !firstMessage.locked.value &&
    !launch.value &&
    !attachments.uploading.value &&
    !attachments.failed.value &&
    (!cwd.value ||
      paths.value.some(
        (p) =>
          p.projectId === selectedProjectId.value &&
          normalizedPath(p.path) === normalizedPath(cwd.value),
      )) &&
    !settings.defaults.loading.value && !settings.defaults.error.value &&
    !settings.config.loading.value && !settings.config.error.value &&
    (!selectedModel.value || choices.value.some((m) => m.model === selectedModel.value)) &&
    (!selectedEffort.value || efforts.value.includes(selectedEffort.value)) &&
    (!(mode.value || profile.value?.mode) || !!modePayload.value) &&
    (!selectedPermission.value ||
      permissionChoices.value.some(
        (profile) => profile.id === selectedPermission.value,
      )) &&
    supportsFields(startTool.value.value, [
      "cwd",
      "projectId",
      ...(selectedModel.value ? ["model"] : []),
      ...(selectedPermission.value ? ["permissions"] : []),
    ]),
);
const create = async () => {
  if (!canCreate.value || !startTool.value.value) return;
  creating.value = true;
  action.error.value = null;
  try {
    if (!cwd.value) {
      projectlessKey.value ||= crypto.randomUUID();
      persistDraft();
      if (action.error.value) return;
    }
    const location = (await serviceTools(client, props.node, [
      { namespace: "agent", interfaceVersion: "1.0.0" },
    ]).call("agent.resolveProject", {
      ...(!cwd.value
        ? { selection: { kind: "projectless", key: projectlessKey.value } }
        : {
            selection: { kind: "existing", cwd: cwd.value },
            expectedProjectId: selectedProjectId.value,
          }),
    })) as Agent.ProjectLocation;
    launch.value = {
      message: message.value,
      effort: selectedEffort.value,
      ...(modePayload.value ? { collaborationMode: modePayload.value } : {}),
      attachments: attachments.staged.value,
      operationId: "",
    };
    persistLaunch();
    if (action.error.value) return;
    await action.start("Create native task", startTool.value.value, {
      cwd: location.cwd,
      projectId: location.project?.nativeId ?? null,
      ...(selectedModel.value ? { model: selectedModel.value } : {}),
      ...(selectedEffort.value ? { config: { model_reasoning_effort: selectedEffort.value } } : {}),
      ...(selectedPermission.value ? { permissions: selectedPermission.value } : {}),
    });
    if (!launch.value?.operationId || action.saved.value?.phase === "failed") {
      launch.value = null;
      persistLaunch();
    } else await continueLaunch();
  } catch (cause) {
    action.error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    creating.value = false;
  }
};
const onMessageKeydown = (event: KeyboardEvent) =>
  submitOnEnter(event, () => void create());
const projectPickerOpen = ref(false);
const projectPickerKind = ref<"normal" | "existing">("normal");
const openProjectPicker = (kind: "normal" | "existing") => {
  projectPickerKind.value = kind;
  projectPickerOpen.value = true;
};
const chooseLocation = async (location: Agent.ProjectLocation) => {
  projectPickerOpen.value = false;
  cwd.value = location.cwd;
  selectedProjectId.value = location.project?.nativeId ?? "";
  await projects.refresh();
};
const createdId = computed(() =>
  action.saved.value?.phase === "succeeded"
    ? text(record(record(action.saved.value.result).thread).id)
    : "",
);
const continueLaunch = async () => {
  if (
    continuing.value ||
    !launch.value ||
    launch.value.operationId !== action.saved.value?.operationId ||
    !createdId.value
  )
    return;
  continuing.value = true;
  const id = createdId.value,
    pending = launch.value;
  try {
    if (pending.message.trim() || pending.attachments.length) {
      const previous = firstMessage.saved.value;
      if (
        previous &&
        record(previous.call.arguments).threadId === id &&
        previous.phase !== "failed"
      ) {
        if (previous.phase !== "succeeded") return;
      } else {
        firstMessage.error.value = null;
        const control = await readNativeThreadControl(
          client,
          props.node,
          id,
          AbortSignal.timeout(35000),
        );
        if (
          !control.attached ||
          text(record(control.thread.status).type) !== "idle"
        )
          throw new Error(
            "The task was created. Its input is not ready yet; continue setup to send the retained first message.",
          );
        const binding = await optionalTool(
          client,
          props.node,
          "codex.turn/start",
        );
        if (!binding || !supportsFields(binding, ["threadId", "input"]))
          throw new Error("This provider cannot send the first message yet.");
        if (
          pending.attachments.some((value) => value.image) &&
          !supportsInputType(binding, "localImage")
        )
          throw new Error(
            "This Codex version cannot receive images with the first message.",
          );
        if (
          pending.effort &&
          !supportsFields(binding, ["threadId", "input", "effort"])
        )
          throw new Error(
            "This provider cannot apply the selected reasoning effort to the first message.",
          );
        await firstMessage.start("Send first message", binding, {
          threadId: id,
          input: messageInput(pending.message, pending.attachments),
          ...(pending.collaborationMode ? { collaborationMode: pending.collaborationMode } : pending.effort ? { effort: pending.effort } : {}),
        });
        if (firstMessage.saved.value?.phase !== "succeeded") return;
      }
    }
    if (message.value === pending.message) {
      message.value = "";
      if (effort.value === pending.effort) effort.value = "";
    }
    if (
      JSON.stringify(attachments.staged.value) ===
      JSON.stringify(pending.attachments)
    )
      attachments.clear();
    launch.value = null;
    projectlessKey.value = "";
    persistLaunch();
    window.location.hash = route("task", { node: props.node, id });
  } catch (cause) {
    firstMessage.error.value =
      cause instanceof Error
        ? cause.message
        : "The first message could not be sent.";
  } finally {
    continuing.value = false;
  }
};
const recoverCreate = async () => {
  await action.reconcile();
  if (action.saved.value?.phase === "failed") {
    launch.value = null;
    persistLaunch();
  } else await continueLaunch();
};
watch([createdId, () => firstMessage.saved.value?.phase], () => {
  const previous = firstMessage.saved.value;
  if (createdId.value && launch.value && (!previous || record(previous.call.arguments).threadId !== createdId.value || previous.phase === 'succeeded'))
    void continueLaunch();
}, { immediate: true });
watch(() => action.saved.value?.phase, (phase) => {
  if (phase === 'failed' && launch.value?.operationId === action.saved.value?.operationId) {
    launch.value = null;
    persistLaunch();
  }
});
</script>
<template>
  <div class="new-task-page mx-auto flex w-full max-w-3xl flex-col px-4">
    <RemoteState
      :loading="status.loading.value"
      :error="status.error.value"
      :has-data="!!status.value.value"
      @retry="status.refresh"
    />
    <div class="new-task-center flex flex-1 flex-col justify-center">
      <h2 class="mb-6 text-center text-2xl font-medium tracking-tight">
        What should we work on?
      </h2>
      <p
        v-if="status.value.value && status.value.value.state !== 'ready'"
        class="mb-4 flex items-center justify-center gap-2 text-sm text-muted-foreground"
      >
        <StatusBadge :label="status.value.value.state" tone="warning" />Codex
        {{ status.value.value.nativeVersion }}
      </p>
      <ConversationComposer
        class="agent-new-task w-full"
        aria-labelledby="create-heading"
        accept-files
        @files="attachments.add"
      >
        <h3 id="create-heading" class="sr-only">Create a new task</h3>
        <ComposerAttachments
          :items="attachments.items.value"
          :disabled="!!launch"
          @remove="attachments.remove"
        />
        <Label for="first-message" class="sr-only">First message</Label>
        <Textarea
          id="first-message"
          v-model="message"
          class="max-h-48 min-h-14 resize-none border-0 bg-transparent px-2 py-2 text-base shadow-none focus-visible:ring-0 dark:bg-transparent"
          :disabled="creating || action.locked.value || !!launch"
          :maxlength="131072"
          placeholder="Ask anything, describe a task, or share an idea…"
          @keydown="onMessageKeydown"
        />

        <div class="composer-footer flex items-center gap-1 pt-1">
          <div class="flex min-w-0 flex-1 flex-wrap items-center gap-0.5">
            <ComposerAddMenu
              files
              :disabled="creating || action.locked.value || !!launch"
              @files="attachments.add"
            />
            <Label for="project" class="sr-only">Project</Label>
            <ProjectSelect
              id="project"
              v-model="cwd"
              :projects="projectOptions"
              :host="status.value.value?.hostId"
              :empty-label="tr('Ohne Projekt', 'No project')"
              class="max-w-full sm:max-w-60"
              :disabled="creating || action.locked.value || !!launch"
              @update:model-value="syncProjectId"
              @add="openProjectPicker('existing')"
              @create="openProjectPicker('normal')"
            />
            <ProjectLocationDialog
              v-model:open="projectPickerOpen"
              :client="client"
              :node="node"
              :host="status.value.value?.hostId"
              :scope="'agent:' + base.href"
              :defaults="projects.value.value?.defaults"
              :kind="projectPickerKind"
              :disabled="action.locked.value"
              @selected="chooseLocation"
            />
            <Label for="model" class="sr-only">Model</Label>
            <OptionSelect
              id="model"
              v-model="model"
              :disabled="action.locked.value || (models.loading.value && !models.value.value)"
            >
              <option value="">{{ choices.find(value => value.model === defaults.model)?.name || defaults.model || 'Loading model…' }}</option>
              <option
                v-for="item in choices.filter((value) => !value.hidden)"
                :key="item.id"
                :value="item.model"
              >
                {{ item.name }}
              </option>
              <optgroup
                v-if="choices.some((value) => value.hidden)"
                label="More models"
              >
                <option
                  v-for="item in choices.filter((value) => value.hidden)"
                  :key="item.id"
                  :value="item.model"
                >
                  {{ item.name }}
                </option>
              </optgroup>
            </OptionSelect>
            <Label for="effort" class="sr-only">Reasoning effort</Label>
            <Label for="mode" class="sr-only">Working mode</Label>
            <OptionSelect id="mode" v-model="mode" :disabled="modes.loading.value && !modes.value.value"><option value="">{{ modes.loading.value && !modes.value.value ? 'Loading modes…' : modeChoices.find(value => value.mode === defaults.mode)?.name || 'Default' }}</option><option v-for="choice in modeChoices" :key="choice.mode" :value="choice.mode">{{ choice.name }}</option></OptionSelect>
            <OptionSelect
              id="effort"
              v-model="effort"
              :disabled="action.locked.value || !efforts.length"
            >
              <option value="">{{ selectedEffort || (efforts.length ? 'Loading effort…' : 'No reasoning effort') }}</option>
              <option v-for="value in efforts" :key="value" :value="value">
                {{ value.charAt(0).toUpperCase() + value.slice(1) }}
              </option>
            </OptionSelect>
            <Label for="permission" class="sr-only">Safety</Label>
            <OptionSelect
              id="permission"
              v-model="permission"
              :disabled="action.locked.value || permissions.loading.value"
            >
              <option value="">{{ defaults.permission ? permissionProfileLabel(defaults.permission) : 'Loading safety…' }}</option>
              <option
                v-for="profile in permissionChoices"
                :key="profile.id"
                :value="profile.id"
              >
                {{ permissionProfileLabel(profile.id) }}
              </option>
            </OptionSelect>
          </div>
          <Button
            size="icon"
            class="size-9 shrink-0 rounded-full"
            :disabled="!canCreate"
            :loading="sending"
            aria-label="Create task"
            @click="create"
            ><ArrowUp aria-hidden="true"
          /></Button>
        </div>

        <p
          v-if="projects.error.value"
          role="alert"
          class="mt-3 px-2 text-sm text-destructive"
        >
          {{ projects.error.value }}
        </p>
        <p
          v-if="startTool.error.value || startTool.value.value === undefined"
          class="mt-3 px-2 text-sm text-muted-foreground"
        >
          Native task creation is unavailable on this provider.
          {{ startTool.error.value }}
        </p>
        <p
          v-if="models.error.value || models.value.value?.unavailable"
          class="mt-3 px-2 text-sm text-muted-foreground"
        >
          Model capabilities unavailable. {{ models.error.value }}
        </p>
        <p v-if="modes.error.value || permissions.error.value || settings.config.error.value || settings.defaults.error.value" role="alert" class="mt-3 px-2 text-sm text-destructive">{{ modes.error.value || permissions.error.value || settings.config.error.value || settings.defaults.error.value }}</p>
        <Button v-if="models.error.value || modes.error.value || permissions.error.value || settings.config.error.value || settings.defaults.error.value" variant="ghost" size="sm" @click="settings.refresh">Reload settings</Button>
        <NativeActionState
          v-if="action.saved.value?.phase !== 'succeeded' || action.error.value"
          :action="action.saved.value"
          :busy="action.busy.value"
          :error="action.error.value"
          @reconcile="recoverCreate"
          @retry="
            async () => {
              await action.retry();
              await continueLaunch();
            }
          "
        />
        <NativeActionState
          v-if="
            firstMessage.saved.value?.phase !== 'succeeded' ||
            firstMessage.error.value
          "
          :action="firstMessage.saved.value"
          :busy="firstMessage.busy.value"
          :error="firstMessage.error.value"
          @reconcile="
            async () => {
              await firstMessage.reconcile();
              await continueLaunch();
            }
          "
          @retry="
            async () => {
              await firstMessage.retry();
              await continueLaunch();
            }
          "
        />
        <div v-if="createdId" class="mt-3 flex gap-2">
          <Button
            v-if="
              launch &&
              !sending &&
              !firstMessage.busy.value &&
              (!firstMessage.saved.value ||
                record(firstMessage.saved.value.call.arguments).threadId !==
                  createdId ||
                ['failed', 'succeeded'].includes(
                  firstMessage.saved.value.phase,
                ))
            "
            variant="outline"
            @click="continueLaunch"
            >Continue setup</Button
          ><Button v-if="!launch" variant="ghost" as-child
            ><a :href="route('task', { node, id: createdId })"
              >Open created task</a
            ></Button
          >
        </div>
      </ConversationComposer>
    </div>
  </div>
</template>

<style scoped>
.new-task-page {
  min-height: calc(100dvh - 3rem - var(--ivy-install-banner-height, 0px));
}
.new-task-center {
  min-height: 360px;
  padding-bottom: clamp(4rem, 18vh, 10rem);
}
@media (max-width: 640px) {
  .new-task-center {
    justify-content: flex-end;
    padding-bottom: max(1rem, env(safe-area-inset-bottom));
  }
  .composer-footer :deep([data-slot="select-trigger"]) {
    max-width: 9rem;
  }
}
</style>
