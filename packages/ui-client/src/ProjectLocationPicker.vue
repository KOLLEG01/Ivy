<script setup lang="ts">
import { ref, useId } from "vue";
import { Alert, AlertDescription, Button, Field, FieldDescription, FieldGroup, FieldLegend, FieldSet, Input, Label, OptionSelect } from "@ivy/ui";
import { serviceTools } from "../../sdk/src/client.js";
import type { Agent, RpcClient } from "../../sdk/src/client.js";
const props = defineProps<{
  client: RpcClient;
  node: string;
  scope: string;
  defaults?: Agent.ProjectsResult["defaults"] | undefined;
  disabled?: boolean;
}>();
const emit = defineEmits<{ selected: [Agent.ProjectLocation] }>();
const typeId = useId(),
  nameId = useId();
const kind = ref<"normal" | "existing">("normal"),
  name = ref(""),
  busy = ref(false),
  error = ref("");
const storageKey = "ivy:project-selection:" + props.scope + ":" + props.node;
const pending = ref<Agent.ProjectSelection | null>(null);
try {
  pending.value = JSON.parse(sessionStorage.getItem(storageKey) ?? "null");
  if (pending.value?.kind === "task" || pending.value?.kind === "internal") pending.value = null;
  if (pending.value) {
    kind.value = pending.value.kind === "existing" ? "existing" : "normal";
    name.value =
      pending.value.kind === "existing"
        ? pending.value.cwd
        : pending.value.name;
  }
} catch {
  error.value = "The saved project selection could not be read.";
}
async function select() {
  if (busy.value || props.disabled) return;
  busy.value = true;
  error.value = "";
  try {
    pending.value ??=
      kind.value === "existing"
        ? { kind: "existing", cwd: name.value }
        : { kind: kind.value, name: name.value, key: crypto.randomUUID() };
    sessionStorage.setItem(storageKey, JSON.stringify(pending.value));
    const location = (await serviceTools(props.client, props.node, [
      { namespace: "agent", interfaceVersion: "1.0.0" },
    ]).call("agent.resolveProject", {
      selection: { ...pending.value },
    })) as Agent.ProjectLocation;
    sessionStorage.removeItem(storageKey);
    pending.value = null;
    emit("selected", location);
  } catch (cause) {
    error.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    busy.value = false;
  }
}
</script>
<template>
  <FieldSet
    class="rounded-lg border p-3"
    :disabled="disabled || busy"
  >
    <FieldLegend>Choose another working location</FieldLegend>
    <FieldGroup>
    <Field>
      <Label :for="typeId">Project type</Label
      ><OptionSelect
        :id="typeId"
        v-model="kind"
        :disabled="!!pending"
        class="w-full"
      >
        <option value="normal">New project</option>
        <option value="existing">Existing directory</option>
      </OptionSelect>
    </Field>
    <Field>
      <Label :for="nameId">{{
        kind === "existing" ? "Absolute directory on this host" : "Project name"
      }}</Label
      ><Input
        :id="nameId"
        v-model="name"
        :disabled="!!pending"
      />
    </Field>
    <FieldDescription
      v-if="defaults && kind !== 'existing'"
      class="break-all text-xs text-muted-foreground"
    >
      Base:
      {{
        defaults.projectRoot
      }}. This project receives its own permanent subdirectory.
    </FieldDescription>
    <Button
      type="button"
      :disabled="!node || (!pending && !name.trim())"
      @click="select"
      >{{ pending ? "Retry original selection" : "Use this location" }}</Button
    >
    <Alert v-if="error" variant="destructive"><AlertDescription>{{ error }}</AlertDescription></Alert>
    </FieldGroup>
  </FieldSet>
</template>
