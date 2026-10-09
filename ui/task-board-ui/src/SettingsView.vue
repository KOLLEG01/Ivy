<script setup lang="ts">
import { computed, ref, toRaw, watch } from "vue";
import {
  Alert,
  AlertDescription,
  Button,
  Field,
  FieldDescription,
  Checkbox,
  Label,
  OptionSelect,
  PageHeader,
  RemoteState,
  SettingsSection,
  useRemote,
} from "@ivy/ui";
import { nativeRead } from "../../../packages/ui-client/src/native";
import { base, client, tr } from "./runtime";
import type { TaskBoard } from "./runtime";
import {
  executionEnvironments,
  executionModels,
  modelAvailability,
  executionSpeedAvailable,
} from "./execution-options";
import { useAction } from "./action";
import ActionState from "./ActionState.vue";

const props = defineProps<{
  workspace: TaskBoard.WorkspaceInfo;
  available: boolean;
}>();
const draft = ref<TaskBoard.Configuration | null>(null),
  baseline = ref(""),
  pin = ref<TaskBoard.ObjectPin | null>(null);
const message = ref("");
const storageError = ref("");
const draftKey = `ivy:task-board:settings-draft:${base.href}:${props.workspace.serviceNodeId}:${props.workspace.callerPrincipalId}:${props.workspace.principalId}:${props.workspace.rootObjectId}`;
try {
  const retained = sessionStorage.getItem(draftKey);
  if (retained) {
    const value = JSON.parse(retained);
    draft.value = value.draft;
    baseline.value = value.baseline;
    pin.value = value.pin;
  }
} catch {
  storageError.value = tr(
    "Der Entwurf kann in diesem Tab nicht gelesen werden.",
    "The draft cannot be read in this tab.",
  );
}
watch(
  [draft, baseline, pin],
  () => {
    try {
      sessionStorage.setItem(
        draftKey,
        JSON.stringify({
          draft: draft.value,
          baseline: baseline.value,
          pin: pin.value,
        }),
      );
    } catch {
      storageError.value = tr(
        "Der Entwurf kann in diesem Tab nicht gespeichert werden.",
        "The draft cannot be retained in this tab.",
      );
    }
  },
  { deep: true },
);
const action = useAction(() => props.workspace, "configuration");
const current = useRemote(
  async (signal) => {
    const result = (await nativeRead(
      client,
      props.workspace.serviceNodeId,
      "task-board.configuration",
      {},
      signal,
    )) as TaskBoard.ConfigurationView;
    if (!draft.value || JSON.stringify(draft.value) === baseline.value) {
      draft.value = structuredClone(result.configuration);
      baseline.value = JSON.stringify(result.configuration);
      pin.value = result.object;
    }
    return result;
  },
  15000,
  ["objects/task-board/configuration"],
);
const environments = useRemote(executionEnvironments, 30000, ["services/agent-manager"]);
const host = computed({
  get: () =>
    draft.value?.defaults.executionRequirement?.kind === "host"
      ? draft.value.defaults.executionRequirement.hostId
      : "",
  set: (value) => {
    if (draft.value)
      draft.value.defaults.executionRequirement = value
        ? { kind: "host", hostId: value }
        : null;
  },
});
const agents = computed(() =>
  (environments.value.value?.agents ?? []).filter(
    (agent) => !host.value || agent.node.hostId === host.value,
  ),
);
const models = computed(() => executionModels(agents.value));
// Models that only some eligible hosts offer stay visible but cannot be the default.
const offered = computed(() =>
  modelAvailability(agents.value).map((entry) => ({
    ...entry,
    name:
      models.value.find((model) => model.model === entry.model)?.name ??
      entry.name,
  })),
);
const efforts = computed(
  () =>
    models.value.find(
      (model) => model.model === draft.value?.defaults.nativeOptions.model,
    )?.efforts ?? [],
);
const speedAvailable = computed(() => executionSpeedAvailable(agents.value, draft.value?.defaults.nativeOptions.model ?? undefined, draft.value?.defaults.nativeOptions.reasoningEffort ?? undefined));
const invalid = computed(
  () =>
    !!draft.value &&
    agents.value.length > 0 &&
    ((!!draft.value.defaults.nativeOptions.model &&
      !models.value.some(
        (model) => model.model === draft.value!.defaults.nativeOptions.model,
      )) ||
      (!!draft.value.defaults.nativeOptions.reasoningEffort &&
        !efforts.value.includes(
          draft.value.defaults.nativeOptions.reasoningEffort,
        )) ||
      (["fast", "flex"].includes(
        draft.value.defaults.nativeOptions.serviceTier ?? "",
      ) &&
        !speedAvailable.value)),
);
watch(
  () => draft.value?.defaults.nativeOptions.model,
  (model, previous) => {
    if (
      draft.value &&
      previous !== undefined &&
      model !== previous &&
      !efforts.value.includes(
        draft.value.defaults.nativeOptions.reasoningEffort ?? "",
      )
    )
      draft.value.defaults.nativeOptions.reasoningEffort = null;
  },
);
const dirty = computed(
  () => !!draft.value && JSON.stringify(draft.value) !== baseline.value,
);
const reload = async () => {
  draft.value = null;
  message.value = "";
  await current.refresh();
};
const recovered = async () => {
  if (action.saved.value?.phase === "succeeded") {
    const request = action.saved.value.call
      .arguments as unknown as TaskBoard.ConfigureRequest;
    baseline.value = JSON.stringify(request.value);
    pin.value = action.saved.value.outcome?.configuration ?? pin.value;
  }
  await current.refresh();
};
const save = async () => {
  if (!draft.value || invalid.value || !dirty.value) return;
  const submitted = structuredClone(toRaw(draft.value));
  message.value = "";
  const result = await action.start(
    tr("TaskBoard-Einstellungen speichern", "Save TaskBoard settings"),
    { action: "configure", configuration: pin.value, value: submitted },
  );
  if (result?.configuration) {
    pin.value = result.configuration;
    baseline.value = JSON.stringify(submitted);
    message.value = tr(
      "Gespeichert. Die Defaults gelten für neue Ausführungen.",
      "Saved. Defaults apply to new executions.",
    );
    await current.refresh();
  }
};
</script>

<template>
  <PageHeader
    :title="tr('TaskBoard-Einstellungen', 'TaskBoard settings')"
    :loading="current.loading.value"
  />
  <section class="mx-auto w-full max-w-4xl">
    <RemoteState
      :loading="current.loading.value"
      :error="current.error.value"
      :has-data="!!draft"
      @retry="current.refresh"
    />
    <template v-if="draft">
      <SettingsSection
        :title="tr('Ausführungsdefaults', 'Execution defaults')"
        :description="
          tr(
            'Neue Agent-Ausführungen übernehmen diese Werte, sofern am Task nichts anderes gewählt ist. Bestehende Unterhaltungen behalten ihre Auswahl.',
            'New agent executions inherit these values unless the task overrides them. Existing conversations retain their selection.',
          )
        "
      >
        <div class="grid gap-4 sm:grid-cols-2 mb-4">
          <Field>
            <Label for="default-updates">{{ tr('Updates', 'Updates') }}</Label>
            <OptionSelect id="default-updates" :model-value="draft.defaults.userContact ?? 'ticket'"
              :disabled="action.locked.value" class="w-full"
              @update:model-value="draft.defaults.userContact = $event as 'ticket' | 'chat' | 'phone'">
              <option value="ticket">{{ tr('Nur Ticket', 'Ticket only') }}</option>
              <option value="chat">{{ tr('Ticket und Chat', 'Ticket and chat') }}</option>
              <option value="phone">{{ tr('Ticket und Anruf', 'Ticket and call') }}</option>
            </OptionSelect>
          </Field>
          <Field>
            <Label>{{ tr('Projektarbeit', 'Project work') }}</Label>
            <Label for="default-use-worktree" class="font-normal"><Checkbox id="default-use-worktree" :model-value="draft.defaults.useWorktree ?? false"
              :disabled="action.locked.value"
              @update:model-value="draft.defaults.useWorktree = $event === true" />{{ tr('Eigenen Worktree verwenden', 'Use a dedicated worktree') }}</Label>
            <Label for="default-allow-parallel" class="font-normal"><Checkbox id="default-allow-parallel" :model-value="draft.defaults.allowParallel ?? false"
              :disabled="action.locked.value"
              @update:model-value="draft.defaults.allowParallel = $event === true" />{{ tr('Parallele Tickets im selben Projekt erlauben', 'Allow parallel tickets in the same project') }}</Label>
            <FieldDescription>{{ tr('Ohne Worktree werden Tickets standardmäßig nacheinander bearbeitet.', 'Without a worktree, tickets run sequentially by default.') }}</FieldDescription>
          </Field>
        </div>
        <div class="grid gap-4 sm:grid-cols-2">
          <Field
            ><Label for="default-host">Host</Label
            ><OptionSelect
              id="default-host"
              v-model="host"
              :disabled="action.locked.value"
              class="w-full"
            >
              <option value="">
                {{
                  tr(
                    "Automatisch · verfügbarer Host",
                    "Automatic · available host",
                  )
                }}
              </option>
              <option
                v-if="host && !environments.value.value?.hosts.includes(host)"
                :value="host"
              >
                {{ host }} · {{ tr("nicht verfügbar", "unavailable") }}
              </option>
              <option
                v-for="value in environments.value.value?.hosts"
                :key="value"
                :value="value"
              >
                {{ value
                }}{{
                  environments.value.value?.agents.some(
                    (agent) => agent.node.hostId === value,
                  )
                    ? ""
                    : " · offline"
                }}
              </option> </OptionSelect
            ><FieldDescription>{{
              tr(
                "Ein ausdrücklich gewählter Host wird bei Ausfall abgewartet.",
                "An explicitly selected host is awaited when offline.",
              )
            }}</FieldDescription></Field
          >
          <Field
            ><Label for="default-model">{{
              tr("Standardmodell", "Default model")
            }}</Label
            ><OptionSelect
              id="default-model"
              :model-value="draft.defaults.nativeOptions.model ?? ''"
              @update:model-value="
                draft.defaults.nativeOptions.model = String($event) || null
              "
              :disabled="action.locked.value || !agents.length"
              class="w-full"
            >
              <option value="">
                {{ tr("Nativer Host-Default", "Native host default") }}
              </option>
              <option
                v-if="
                  draft.defaults.nativeOptions.model &&
                  !models.some(
                    (model) =>
                      model.model === draft!.defaults.nativeOptions.model,
                  )
                "
                :value="draft.defaults.nativeOptions.model"
              >
                {{ draft.defaults.nativeOptions.model }} ·
                {{ tr("nicht verfügbar", "unavailable") }}
              </option>
              <option
                v-for="model in offered.filter((value) => !value.hidden)"
                :key="model.model"
                :value="model.model"
                :disabled="model.missing.length > 0"
              >
                {{ model.name
                }}{{
                  model.missing.length
                    ? " · " + tr("fehlt auf", "not on") + " " + model.missing.join(", ")
                    : ""
                }}
              </option>
              <optgroup
                v-if="offered.some((value) => value.hidden)"
                :label="tr('Weitere Modelle', 'More models')"
              >
                <option
                v-for="model in offered.filter((value) => value.hidden)"
                :key="model.model"
                :value="model.model"
                :disabled="model.missing.length > 0"
              >
                {{ model.name
                }}{{
                  model.missing.length
                    ? " · " + tr("fehlt auf", "not on") + " " + model.missing.join(", ")
                    : ""
                }}
              </option>
              </optgroup>
            </OptionSelect></Field
          >
          <Field
            ><Label for="default-reasoning">Reasoning</Label
            ><OptionSelect
              id="default-reasoning"
              :model-value="draft.defaults.nativeOptions.reasoningEffort ?? ''"
              @update:model-value="
                draft.defaults.nativeOptions.reasoningEffort =
                  String($event) || null
              "
              :disabled="
                action.locked.value ||
                !draft.defaults.nativeOptions.model ||
                !agents.length
              "
              class="w-full"
            >
              <option value="">
                {{ tr("Modell-Default", "Model default") }}
              </option>
              <option
                v-if="
                  draft.defaults.nativeOptions.reasoningEffort &&
                  !efforts.includes(
                    draft.defaults.nativeOptions.reasoningEffort,
                  )
                "
                :value="draft.defaults.nativeOptions.reasoningEffort"
              >
                {{ draft.defaults.nativeOptions.reasoningEffort }} ·
                {{ tr("nicht verfügbar", "unavailable") }}
              </option>
              <option v-for="effort in efforts" :key="effort" :value="effort">
                {{ effort }}
              </option>
            </OptionSelect></Field
          >
          <Field
            ><Label for="default-speed">Speed</Label
            ><OptionSelect
              id="default-speed"
              :model-value="
                draft.defaults.nativeOptions.serviceTier ?? 'standard'
              "
              @update:model-value="
                draft.defaults.nativeOptions.serviceTier = $event as
                  'standard' | 'fast' | 'flex'
              "
              :disabled="action.locked.value"
              class="w-full"
            >
              <option value="standard">Standard</option>
              <option value="fast" :disabled="!speedAvailable">Fast</option>
              <option value="flex" :disabled="!speedAvailable">Flex</option>
            </OptionSelect></Field
          >
        </div>
        <Alert
          v-if="invalid || environments.error.value"
          variant="destructive"
          class="mt-4"
          ><AlertDescription>{{
            environments.error.value ||
            tr(
              "Die Auswahl wird von den verfügbaren Hosts nicht unterstützt.",
              "Available hosts do not support this selection.",
            )
          }}</AlertDescription></Alert
        >
      </SettingsSection>
      <Alert v-if="storageError" variant="destructive" class="mb-4"
        ><AlertDescription>{{ storageError }}</AlertDescription></Alert
      >
      <ActionState :action="action" @changed="recovered" />
      <div class="flex flex-wrap items-center justify-end gap-3 border-t py-3">
        <p
          v-if="message"
          role="status"
          class="mr-auto text-sm text-muted-foreground"
        >
          {{ message }}
        </p>
        <Button
          variant="outline"
          :disabled="action.busy.value"
          @click="reload"
          >{{ tr("Neu laden", "Reload") }}</Button
        >
        <Button
          :disabled="
            !available ||
            workspace.role !== 'user' ||
            action.locked.value ||
            !!storageError ||
            !!current.error.value ||
            !dirty ||
            invalid
          "
          @click="save"
          >{{ tr("Speichern", "Save") }}</Button
        >
      </div>
    </template>
  </section>
</template>
