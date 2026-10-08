<script setup lang="ts">
import { computed, ref, watch } from "vue";
import {
  Button,
  ChoiceChips,
  HostMark,
  Label,
  OptionSelect,
  RemoteState,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  useRemote,
} from "@ivy/ui";
import type { Agent, Operation } from "../../../packages/sdk/src/client.js";
import { newOperationId } from "../../../packages/sdk/src/client.js";
import {
  nativeRead,
  serviceNodeLabel,
  permissionProfilesFrom,
  permissionProfileLabel,
  optionalTool,
} from "../../../packages/ui-client/src/native";
import { nativeModesFrom } from "../../../packages/ui-client/src/native-modes";
import {
  emptyExecutionDefaults,
  executionDefaultsPath,
  effectiveNativeSettings,
  nativeConfiguredHome,
  useNativeSettings,
} from "../../../packages/ui-client/src/native-settings";
import { client, tr } from "./runtime";

const type = ref<"codex" | "claude">("codex");
const hosts = useRemote(
  async (signal) => {
    const page = await client.request(
      "serviceNodes.list",
      { serviceName: "agent-manager", limit: 100 },
      { signal },
    );
    const results = await Promise.allSettled(
      page.items.map(async (node) => ({
        node: node.serviceNodeId,
        label: serviceNodeLabel(page.items, node.serviceNodeId),
        status: (await nativeRead(
          client,
          node.serviceNodeId,
          "agent.status",
          {},
          AbortSignal.any([signal, AbortSignal.timeout(5000)]),
        )) as Agent.Status,
      })),
    );
    return results.flatMap((value) =>
      value.status === "fulfilled" && value.value.status.state === "ready"
        ? [value.value]
        : [],
    );
  },
  30000,
  ["services"],
);
const selectedNode = ref("");
const matchingHosts = computed(
  () =>
    hosts.value.value?.filter(
      (host) => host.status.serverType === type.value,
    ) ?? [],
);
watch(matchingHosts, (values) => {
  if (!values.some((host) => host.node === selectedNode.value))
    selectedNode.value = values[0]?.node ?? "";
});
const settings = useNativeSettings(
  client,
  () => selectedNode.value,
  () =>
    nativeConfiguredHome(
      hosts.value.value?.find((host) => host.node === selectedNode.value)
        ?.status,
    ),
);
watch(selectedNode, () => settings.refresh());
const sendTool = useRemote(
  () =>
    selectedNode.value
      ? optionalTool(client, selectedNode.value, "codex.turn/start")
      : Promise.resolve(undefined),
  0,
  ["services"],
);
watch(selectedNode, () => void sendTool.refresh());
const modeChoices = computed(() =>
  nativeModesFrom(sendTool.value.value, settings.modes.value.value),
);
const profiles = ref<Agent.ExecutionDefaultsDocument>({
  schemaVersion: 1,
  codex: emptyExecutionDefaults(),
  claude: emptyExecutionDefaults(),
});
const dirty = ref(false),
  saving = ref(false),
  error = ref(""),
  message = ref("");
const pending = ref<Operation.ObjectsWriteParams | null>(null);
watch(
  () => settings.defaults.value.value,
  (value) => {
    if (!dirty.value && !pending.value)
      profiles.value = value?.document
        ? structuredClone(value.document)
        : {
            schemaVersion: 1,
            codex: emptyExecutionDefaults(),
            claude: emptyExecutionDefaults(),
          };
  },
);
const profile = computed(() => profiles.value[type.value]);
const inherited = computed(() =>
  effectiveNativeSettings(
    settings.models.value.value,
    settings.config.value.value,
    null,
  ),
);
const model = computed(() => profile.value.model || inherited.value.model);
const inheritedEffort = computed(
  () =>
    effectiveNativeSettings(
      settings.models.value.value,
      settings.config.value.value,
      null,
      { model: profile.value.model },
    ).effort,
);
const efforts = computed(
  () =>
    settings.choices.value.find((choice) => choice.model === model.value)
      ?.efforts ?? [],
);
const permissionChoices = computed(() =>
  permissionProfilesFrom(settings.permissions.value.value).filter(
    (choice) => choice.allowed,
  ),
);
const selectedHost = computed(() =>
  matchingHosts.value.find((host) => host.node === selectedNode.value),
);
const overridden = computed(() =>
  Object.values(profile.value).some((value) => value),
);
const inheritAll = () => {
  for (const field of Object.keys(
    profile.value,
  ) as (keyof Agent.ExecutionDefaults)[])
    profile.value[field] = null;
  dirty.value = true;
};
const set = (field: keyof Agent.ExecutionDefaults, value: string) => {
  profile.value[field] = value || null;
  dirty.value = true;
  if (
    field === "model" &&
    profile.value.effort &&
    !efforts.value.includes(profile.value.effort)
  )
    profile.value.effort = null;
};
async function save() {
  if (saving.value) return;
  saving.value = true;
  error.value = "";
  try {
    const object = settings.defaults.value.value?.object;
    pending.value ??= {
      mutationId: await newOperationId(client),
      contractVersion: "1.0.0",
      references: {},
      content: {
        encoding: "json",
        value: JSON.parse(JSON.stringify(profiles.value)),
      },
      ...(object
        ? { objectId: object.id, expectedRevision: object.currentRevision }
        : {
            create: {
              contractKey: "agent/execution-defaults",
              parentId: null,
              ownerObjectId: null,
              name: executionDefaultsPath.slice(1),
            },
          }),
    };
    await client.request("objects.write", pending.value);
    pending.value = null;
    dirty.value = false;
    message.value = tr(
      "Gespeichert. Neue Tasks verwenden die Defaults ihres Servertyps.",
      "Saved. New tasks use the defaults of their server type.",
    );
    await settings.defaults.refresh();
  } catch (cause) {
    if (
      [
        "revision_conflict",
        "name_conflict",
        "invalid_arguments",
        "contract_mismatch",
        "forbidden",
      ].includes((cause as { code?: string }).code ?? "")
    )
      pending.value = null;
    error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <div class="space-y-4">
    <p class="text-sm text-muted-foreground">
      {{
        tr(
          "Leere Vorgaben übernehmen die aktuelle Serverkonfiguration. Bestehende Tasks behalten ihre Einstellungen.",
          "Inherited values follow the current server configuration. Existing tasks retain their settings.",
        )
      }}
    </p>
    <Tabs
      :model-value="type"
      class="gap-4"
      @update:model-value="type = $event === 'claude' ? 'claude' : 'codex'"
    >
      <TabsList :aria-label="tr('Servertyp', 'Server type')">
        <TabsTrigger value="codex">Codex</TabsTrigger>
        <TabsTrigger value="claude">Claude</TabsTrigger>
      </TabsList>
      <TabsContent :value="type" class="space-y-4">
        <RemoteState
          :loading="settings.defaults.loading.value"
          :error="settings.defaults.error.value || hosts.error.value"
          :has-data="!!settings.defaults.value.value"
          @retry="settings.refresh"
        />
        <div v-if="matchingHosts.length > 1" class="space-y-2">
          <p class="text-sm font-medium">
            {{ tr("Auswahl laut Host", "Choices from host") }}
          </p>
          <ChoiceChips
            v-model="selectedNode"
            :label="tr('Auswahl laut Host', 'Choices from host')"
            :options="
              matchingHosts.map((host) => ({
                value: host.node,
                label: host.label,
              }))
            "
          />
        </div>
        <p
          v-else-if="selectedHost"
          class="flex items-center gap-2 text-sm text-muted-foreground"
        >
          <HostMark :label="selectedHost.label" :seed="selectedHost.node" />
          {{ tr("Auswahl laut", "Choices from") }} {{ selectedHost.label }}
        </p>
        <p v-else-if="hosts.value.value" class="text-sm text-muted-foreground">
          {{
            tr(
              "Kein passender Host ist verbunden. Gespeicherte Werte bleiben erhalten; weitere Auswahl erscheint, sobald ein Host bereit ist.",
              "No matching host is connected. Saved values are kept; more choices appear once a host is ready.",
            )
          }}
        </p>
        <fieldset
          :disabled="saving || !!pending || settings.defaults.loading.value"
          class="grid min-w-0 gap-3 sm:grid-cols-2"
        >
          <Label class="flex-col items-start gap-2"
            >{{ tr("Modell", "Model") }}
            <OptionSelect
              :model-value="profile.model || ''"
              aria-label="Default model for server type"
              @update:model-value="set('model', String($event))"
            >
              <option value="">
                {{ inherited.model || tr("Servervorgabe", "Server setting") }} ·
                {{ tr("vom Server", "from server") }}
              </option>
              <option
                v-if="
                  profile.model &&
                  !settings.choices.value.some(
                    (choice) => choice.model === profile.model,
                  )
                "
                :value="profile.model"
              >
                {{ profile.model }}
              </option>
              <option
                v-for="choice in settings.choices.value"
                :key="choice.id"
                :value="choice.model"
              >
                {{ choice.name }}
              </option>
            </OptionSelect>
          </Label>
          <Label class="flex-col items-start gap-2"
            >{{ tr("Arbeitsmodus", "Working mode") }}
            <OptionSelect
              :model-value="profile.mode || ''"
              aria-label="Default mode for server type"
              @update:model-value="set('mode', String($event))"
            >
              <option value="">
                {{
                  modeChoices.find((choice) => choice.mode === inherited.mode)
                    ?.name || inherited.mode
                }}
                · {{ tr("vom Server", "from server") }}
              </option>
              <option
                v-if="
                  profile.mode &&
                  !modeChoices.some((choice) => choice.mode === profile.mode)
                "
                :value="profile.mode"
              >
                {{ profile.mode }}
              </option>
              <option
                v-for="choice in modeChoices"
                :key="choice.mode"
                :value="choice.mode"
              >
                {{ choice.name }}
              </option>
            </OptionSelect>
          </Label>
          <Label class="flex-col items-start gap-2"
            >{{ tr("Denkaufwand", "Reasoning effort") }}
            <OptionSelect
              :model-value="profile.effort || ''"
              aria-label="Default effort for server type"
              @update:model-value="set('effort', String($event))"
            >
              <option value="">
                {{
                  inheritedEffort ||
                  tr("Kein Denkaufwand", "No reasoning effort")
                }}
                · {{ tr("vom Server", "from server") }}
              </option>
              <option
                v-if="profile.effort && !efforts.includes(profile.effort)"
                :value="profile.effort"
              >
                {{ profile.effort }}
              </option>
              <option v-for="choice in efforts" :key="choice" :value="choice">
                {{ choice }}
              </option>
            </OptionSelect>
          </Label>
          <Label class="flex-col items-start gap-2"
            >{{ tr("Berechtigungen", "Safety") }}
            <OptionSelect
              :model-value="profile.permission || ''"
              aria-label="Default safety for server type"
              @update:model-value="set('permission', String($event))"
            >
              <option value="">
                {{
                  inherited.permission
                    ? permissionProfileLabel(inherited.permission)
                    : tr("Servervorgabe", "Server setting")
                }}
                · {{ tr("vom Server", "from server") }}
              </option>
              <option
                v-if="
                  profile.permission &&
                  !permissionChoices.some(
                    (choice) => choice.id === profile.permission,
                  )
                "
                :value="profile.permission"
              >
                {{ profile.permission }}
              </option>
              <option
                v-for="choice in permissionChoices"
                :key="choice.id"
                :value="choice.id"
              >
                {{ permissionProfileLabel(choice.id) }}
              </option>
            </OptionSelect>
          </Label>
        </fieldset>
        <p
          v-if="
            settings.models.error.value ||
            settings.modes.error.value ||
            settings.permissions.error.value ||
            settings.config.error.value
          "
          role="alert"
          class="text-sm text-destructive"
        >
          {{
            settings.models.error.value ||
            settings.modes.error.value ||
            settings.permissions.error.value ||
            settings.config.error.value
          }}
        </p>
        <Button
          v-if="overridden"
          variant="link"
          size="sm"
          class="h-auto px-0"
          :disabled="saving || !!pending"
          @click="inheritAll"
          >{{
            tr(
              "Alles vom Server übernehmen",
              "Inherit everything from the server",
            )
          }}</Button
        >
      </TabsContent>
    </Tabs>
    <div class="flex gap-2">
      <Button
        :disabled="
          saving ||
          (!dirty && !pending) ||
          settings.defaults.loading.value ||
          !!settings.defaults.error.value
        "
        @click="save"
        >{{
          pending
            ? tr("Speichern erneut versuchen", "Retry original save")
            : tr("Defaults speichern", "Save defaults")
        }}</Button
      ><Button
        variant="ghost"
        :disabled="saving || !!pending"
        @click="settings.refresh"
        >{{ tr("Neu laden", "Reload") }}</Button
      >
    </div>
    <p v-if="error" role="alert" class="text-sm text-destructive">
      {{ error }}
    </p>
    <p v-if="message" role="status" class="text-sm">{{ message }}</p>
  </div>
</template>
