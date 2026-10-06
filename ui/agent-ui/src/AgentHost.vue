<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  Button,
  ComposerAddMenu,
  ComposerAttachments,
  ConversationComposer,
  Label,
  OptionSelect,
  Popover,
  PopoverContent,
  PopoverTrigger,
  RemoteState,
  StatusBadge,
  Textarea,
  useRemote,
  submitOnEnter,
} from "@ivy/ui";
import { ArrowUp, FolderPlus } from "@lucide/vue";
import type { Agent } from "../../../packages/sdk/src/client.js";
import { route } from "../../../packages/ui-client/src/runtime";
import {
  modelsFrom,
  nativeModels,
  nativePermissionProfiles,
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
import NativeActionState from "../../../packages/ui-client/src/NativeActionState.vue";
import { base, client } from "./runtime";
import ProjectLocationPicker from "../../../packages/ui-client/src/ProjectLocationPicker.vue";
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
const models = useRemote(async (signal) => {
  const binding = await optionalTool(client, props.node, "codex.model/list");
  if (!binding) return { data: [], unavailable: true, nextCursor: null };
  return nativeModels(client, props.node, signal);
});
const startTool = useRemote(() =>
  optionalTool(client, props.node, "codex.thread/start"),
);
const choices = computed(() => modelsFrom(models.value.value)),
  model = ref(""),
  effort = ref(""),
  permission = ref(""),
  cwd = ref(props.project),
  selectedProjectId = ref(props.projectId),
  message = ref("");
const efforts = computed(
  () =>
    choices.value.find((value) => value.model === model.value)?.efforts ?? [],
);
const permissions = useRemote(async (signal) =>
  (await optionalTool(client, props.node, "codex.permissionProfile/list"))
    ? nativePermissionProfiles(client, props.node, cwd.value, signal)
    : { data: [], unavailable: true },
);
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
const attachments = useMessageAttachments(client, () => props.node);
const launchKey = "ivy:agent-launch:" + base.href + props.node;
const launch = ref<{
  message: string;
  effort: string;
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
  effort.value = text(draft.effort);
  permission.value = text(draft.permission);
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
watch([model, effort, permission, cwd, selectedProjectId, message, attachments.staged], () => {
  try {
    sessionStorage.setItem(
      draftKey,
      JSON.stringify({
        model: model.value,
        effort: effort.value,
        permission: permission.value,
        cwd: cwd.value,
        projectId: selectedProjectId.value,
        message: message.value,
        attachments: attachments.staged.value,
      }),
    );
  } catch {
    action.error.value = "The new task draft cannot be retained in this tab.";
  }
});
const paths = computed(() =>
  projectChoices(projects.value.value?.projects ?? []),
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
    !firstMessage.locked.value &&
    !launch.value &&
    !attachments.uploading.value &&
    !attachments.failed.value &&
    !!cwd.value &&
    paths.value.some(
      (p) =>
        p.projectId === selectedProjectId.value &&
        normalizedPath(p.path) === normalizedPath(cwd.value),
    ) &&
    (!model.value || choices.value.some((m) => m.model === model.value)) &&
    (!effort.value || efforts.value.includes(effort.value)) &&
    (!permission.value ||
      permissionChoices.value.some(
        (profile) => profile.id === permission.value,
      )) &&
    supportsFields(startTool.value.value, [
      "cwd",
      "projectId",
      ...(model.value ? ["model"] : []),
      ...(permission.value ? ["permissions"] : []),
    ]),
);
const create = async () => {
  if (!canCreate.value || !startTool.value.value) return;
  try {
    const location = (await serviceTools(client, props.node, [
      { namespace: "agent", interfaceVersion: "1.0.0" },
    ]).call("agent.resolveProject", {
      selection: { kind: "existing", cwd: cwd.value },
      expectedProjectId: selectedProjectId.value,
    })) as Agent.ProjectLocation;
    launch.value = {
      message: message.value,
      effort: effort.value,
      attachments: attachments.staged.value,
      operationId: "",
    };
    persistLaunch();
    if (action.error.value) return;
    await action.start("Create native task", startTool.value.value, {
      cwd: location.cwd,
      projectId: location.project.nativeId,
      ...(model.value ? { model: model.value } : {}),
      ...(permission.value ? { permissions: permission.value } : {}),
    });
    if (!launch.value?.operationId || action.saved.value?.phase === "failed") {
      launch.value = null;
      persistLaunch();
    } else await continueLaunch();
  } catch (cause) {
    action.error.value = cause instanceof Error ? cause.message : String(cause);
  }
};
const onMessageKeydown = (event: KeyboardEvent) =>
  submitOnEnter(event, () => void create());
const projectPickerOpen = ref(false);
const chooseLocation = async (location: Agent.ProjectLocation) => {
  projectPickerOpen.value = false;
  cwd.value = location.cwd;
  selectedProjectId.value = location.project.nativeId;
  await projects.refresh();
};
const createdId = computed(() =>
  action.saved.value?.phase === "succeeded"
    ? text(record(record(action.saved.value.result).thread).id)
    : "",
);
const continueLaunch = async () => {
  if (
    !launch.value ||
    launch.value.operationId !== action.saved.value?.operationId ||
    !createdId.value
  )
    return;
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
          throw new Error("This Codex version cannot receive images with the first message.");
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
          ...(pending.effort ? { effort: pending.effort } : {}),
        });
        if (firstMessage.saved.value?.phase !== "succeeded") return;
      }
    }
    if (message.value === pending.message) {
      message.value = "";
      if (effort.value === pending.effort) effort.value = "";
    }
    if (JSON.stringify(attachments.staged.value) === JSON.stringify(pending.attachments))
      attachments.clear();
    launch.value = null;
    persistLaunch();
    window.location.hash = route("task", { node: props.node, id });
  } catch (cause) {
    firstMessage.error.value =
      cause instanceof Error
        ? cause.message
        : "The first message could not be sent.";
  }
};
const recoverCreate = async () => {
  await action.reconcile();
  if (action.saved.value?.phase === "failed") {
    launch.value = null;
    persistLaunch();
  } else await continueLaunch();
};
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
          :disabled="action.locked.value || !!launch"
          :maxlength="131072"
          placeholder="Ask anything, describe a task, or share an idea…"
          @keydown="onMessageKeydown"
        />

        <div class="composer-footer flex items-center gap-1 pt-1">
          <div class="flex min-w-0 flex-1 flex-wrap items-center gap-0.5">
            <ComposerAddMenu
              files
              :disabled="action.locked.value || !!launch"
              @files="attachments.add"
            />
            <Label for="project" class="sr-only">Project</Label>
            <OptionSelect
              id="project"
              v-model="cwd"
              class="max-w-full sm:max-w-60"
              :disabled="action.locked.value"
              @update:model-value="syncProjectId"
            >
              <option value="">Choose project</option>
              <option
                v-for="path in paths"
                :key="path.projectId + ':' + path.path"
                :value="path.path"
              >
                {{ path.label }}
              </option>
            </OptionSelect>
            <Popover v-model:open="projectPickerOpen">
              <PopoverTrigger as-child>
                <Button
                  variant="ghost"
                  size="icon-sm"
                  class="size-7 rounded-full text-muted-foreground"
                  aria-label="Add project"
                  :disabled="action.locked.value"
                  ><FolderPlus aria-hidden="true"
                /></Button>
              </PopoverTrigger>
              <PopoverContent
                align="start"
                class="w-[min(32rem,calc(100vw-2rem))]"
              >
                <p class="mb-3 text-sm font-medium">Add project</p>
                <ProjectLocationPicker
                  :client="client"
                  :node="node"
                  :scope="'agent:' + base.href"
                  :defaults="projects.value.value?.defaults"
                  :disabled="action.locked.value"
                  @selected="chooseLocation"
                />
              </PopoverContent>
            </Popover>
            <Label for="model" class="sr-only">Model</Label>
            <OptionSelect
              id="model"
              v-model="model"
              :disabled="action.locked.value"
            >
              <option value="">Default model</option>
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
            <OptionSelect
              id="effort"
              v-model="effort"
              :disabled="action.locked.value || !model"
            >
              <option value="">Default effort</option>
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
              <option value="">Default safety</option>
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
