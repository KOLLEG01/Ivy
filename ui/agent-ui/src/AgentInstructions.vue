<script setup lang="ts">
import { computed, ref, useId, watch } from "vue";
import {
  Alert,
  AlertDescription,
  Button,
  Checkbox,
  Label,
  Switch,
  Textarea,
} from "@ivy/ui";
import { newOperationId } from "../../../packages/sdk/src/client.js";
import type { Agent, Operation } from "../../../packages/sdk/src/client.js";
import { client } from "./runtime";

const props = defineProps<{
  hostId: string | null;
  status?: Agent.InstructionsStatus | undefined;
}>();
const text = ref("");
const overrideDefaults = ref(false);
const enabled = ref(true);
const includeLocal = ref(true);
const busy = ref(false);
const loaded = ref(false);
const error = ref("");
const message = ref("");
const overrideId = useId();
let object: Operation.ObjectMetadata | null = null;
let pending: Operation.ObjectsWriteParams | null = null;

const sourceLabel = computed(() => {
  if (!overrideDefaults.value)
    return props.hostId === null
      ? "Packaged default template"
      : "Inherited Hive or packaged instructions";
  if (!enabled.value)
    return props.hostId === null
      ? "Hive override disables managed instructions"
      : "Managed instructions disabled for this host";
  return props.hostId === null
    ? "Hive AGENTS.md override"
    : "Inherited instructions plus host override";
});

async function name() {
  if (props.hostId === null) return "ivy-agent-instructions";
  const bytes = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(props.hostId),
  );
  return (
    "ivy-agent-instructions-host-" +
    [...new Uint8Array(bytes)]
      .map((value) => value.toString(16).padStart(2, "0"))
      .join("")
  );
}

async function load() {
  if (busy.value) return;
  busy.value = true;
  error.value = "";
  loaded.value = false;
  try {
    try {
      object = await client.request("objects.stat", {
        path: "/" + (await name()),
      });
    } catch (failure) {
      if ((failure as { code?: string }).code !== "not_found") throw failure;
      object = null;
    }
    if (object) {
      const value = await client.request("objects.read", {
        objectId: object.id,
        revision: object.currentRevision,
      });
      if (
        value.object.contractKey !== "agent/instructions" ||
        value.revision.contractVersion !== "1.0.0" ||
        value.content.encoding !== "json"
      )
        throw new Error(
          "The instructions path contains another document type.",
        );
      const document = value.content.value as Agent.InstructionsDocument;
      if (document.hostId !== props.hostId || typeof document.text !== "string")
        throw new Error("Instruction scope does not match.");
      text.value = document.text;
      overrideDefaults.value = document.enabled !== null;
      enabled.value = document.enabled !== false;
      includeLocal.value = document.includeLocal;
      message.value =
        document.enabled === null
          ? props.hostId === null
            ? `Saved revision ${object.currentRevision}; the packaged template is active.`
            : `Saved revision ${object.currentRevision}; inherited instructions are active.`
          : `Saved revision ${object.currentRevision}.`;
    } else {
      text.value = "";
      overrideDefaults.value = false;
      enabled.value = true;
      includeLocal.value = true;
      message.value =
        props.hostId === null
          ? "The packaged instruction template is active."
          : "This host inherits the Hive or packaged instructions.";
    }
    loaded.value = true;
  } catch (failure) {
    error.value = failure instanceof Error ? failure.message : String(failure);
  } finally {
    busy.value = false;
  }
}

async function save(remove = false) {
  if (busy.value || !loaded.value) return;
  busy.value = true;
  error.value = "";
  try {
    const inherit = remove || !overrideDefaults.value;
    pending ??= {
      mutationId: await newOperationId(client),
      contractVersion: "1.0.0",
      references: {},
      content: {
        encoding: "json",
        value: {
          schemaVersion: 1,
          hostId: props.hostId,
          enabled: inherit ? null : enabled.value,
          includeLocal: includeLocal.value,
          text: remove ? "" : text.value,
        },
      },
      ...(object
        ? { objectId: object.id, expectedRevision: object.currentRevision }
        : {
            create: {
              contractKey: "agent/instructions",
              parentId: null,
              ownerObjectId: null,
              name: await name(),
            },
          }),
    };
    const result = await client.request("objects.write", pending);
    object = result.object;
    pending = null;
    if (remove) {
      overrideDefaults.value = false;
      enabled.value = true;
      text.value = "";
    }
    message.value = inherit
      ? `Saved revision ${result.revision.revision}; ${
          props.hostId === null
            ? "the packaged template is active"
            : "inherited instructions are active"
        }.`
      : `Saved revision ${result.revision.revision}. Hosts synchronize automatically; running turns keep their existing context.`;
  } catch (failure) {
    const code = (failure as { code?: string }).code;
    if (
      [
        "revision_conflict",
        "name_conflict",
        "invalid_arguments",
        "contract_mismatch",
        "forbidden",
      ].includes(code ?? "")
    )
      pending = null;
    error.value = failure instanceof Error ? failure.message : String(failure);
  } finally {
    busy.value = false;
  }
}

watch(
  () => props.hostId,
  () => {
    pending = null;
    void load();
  },
  { immediate: true },
);
</script>

<template>
  <section
    class="space-y-4"
    :aria-label="hostId === null ? 'Hive instructions' : 'Host instructions'"
  >
    <h3 class="sr-only">
      {{ hostId === null ? "Hive AGENTS.md" : "Instructions for this host" }}
    </h3>

    <div class="rounded-lg border bg-muted/35 px-3 py-2 text-sm">
      <span class="text-muted-foreground">Selected source:</span>
      <strong class="ml-1 font-medium">{{ sourceLabel }}</strong>
    </div>

    <div class="flex items-center justify-between gap-4 rounded-lg border p-3">
      <Label
        :for="overrideId"
        class="min-w-0 cursor-pointer flex-col items-start gap-1"
      >
        <span class="block text-sm font-medium">
          {{
            hostId === null
              ? "Override packaged defaults"
              : "Use host-specific instructions"
          }}
        </span>
        <span class="block text-xs font-normal text-muted-foreground">
          {{
            hostId === null
              ? "Use the Hive AGENTS.md below instead of the packaged template."
              : "Append host guidance after inherited instructions with higher precedence."
          }}
        </span>
      </Label>
      <Switch
        :id="overrideId"
        v-model="overrideDefaults"
        :disabled="busy || !loaded || !!pending"
        :aria-label="
          hostId === null
            ? 'Override packaged defaults'
            : 'Use host-specific instructions'
        "
      />
    </div>

    <fieldset
      :disabled="busy || !loaded || !!pending || !overrideDefaults"
      class="space-y-3"
    >
      <Label class="flex items-center gap-2"
        ><Checkbox v-model="enabled" />Apply managed instructions</Label
      >
      <p v-if="!enabled" class="text-xs text-muted-foreground">
        This disables managed instructions instead of falling back to the
        inherited source.
      </p>
      <Label class="flex items-center gap-2"
        ><Checkbox v-model="includeLocal" />Prepend each host's existing local
        AGENTS.md</Label
      >
      <Label class="flex-col items-start gap-2"
        >{{ hostId === null ? "Hive AGENTS.md" : "Host instruction text"
        }}<Textarea
          v-model="text"
          rows="8"
          maxlength="16384"
          class="font-mono text-sm font-normal"
      /></Label>
    </fieldset>

    <div class="flex flex-wrap gap-2">
      <Button
        :disabled="
          busy ||
          !loaded ||
          (!overrideDefaults && !object) ||
          (!text.trim() && overrideDefaults && enabled)
        "
        @click="save(false)"
        >{{ pending ? "Retry original save" : "Save instructions" }}</Button
      >
      <Button
        variant="outline"
        :disabled="busy || !loaded || !!pending || !object"
        @click="save(true)"
        >{{
          hostId === null
            ? "Use packaged default"
            : "Use inherited instructions"
        }}</Button
      >
      <Button variant="ghost" :disabled="busy || !!pending" @click="load"
        >Reload saved revision</Button
      >
    </div>
    <Alert v-if="error" variant="destructive"><AlertDescription>{{ error }}</AlertDescription></Alert>
    <p v-if="message" role="status" class="text-sm">{{ message }}</p>
    <div v-if="status" class="space-y-1 text-sm text-muted-foreground">
      <p>
        Local application: {{ status.state
        }}{{ status.code ? " · " + status.code : "" }}
      </p>
      <p class="break-all">Home: {{ status.home }}</p>
      <p class="break-all">
        Applied version: {{ status.appliedVersion ?? "none" }}
      </p>
    </div>
    <p class="text-xs text-muted-foreground">
      AgentManager writes the selected result atomically to AGENTS.override.md.
      It never edits the local AGENTS.md. Running turns and already loaded tasks
      keep their existing context.
    </p>
  </section>
</template>
