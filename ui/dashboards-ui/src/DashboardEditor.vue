<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { ExternalLink, Save } from "@lucide/vue";
import {
  Button,
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
  Field,
  FieldDescription,
  FieldError,
  FieldLabel,
  Input,
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
  Textarea,
  ToolbarContent,
} from "@ivy/ui";
import { base, starter, tr } from "./runtime";
import type { Dashboard, Row } from "./runtime";
import Viewer from "./Viewer.vue";

// Edits one dashboard beside a live preview of the draft HTML; data sources apply once saved.
const props = defineProps<{ node: string; row: Row | null; busy: boolean }>();
const emit = defineEmits<{ save: [value: Dashboard]; cancel: [] }>();
const title = ref(props.row?.value.title ?? ""),
  html = ref(props.row?.value.html ?? starter),
  sources = ref(JSON.stringify(props.row?.value.sources ?? {}, null, 2)),
  refreshSeconds = ref(props.row?.value.refreshSeconds ?? 5),
  metadata = ref(JSON.stringify(props.row?.value.metadata ?? {}, null, 2)),
  imageToken = ref(props.row?.value.imageToken ?? null);
const parsedMetadata = computed(() => {
  try {
    const value = JSON.parse(metadata.value || "{}");
    if (
      !value ||
      typeof value !== "object" ||
      Array.isArray(value) ||
      Object.hasOwn(value, "refreshSeconds") ||
      encodeURIComponent(JSON.stringify(value)).length > 8000
    )
      throw Error(
        tr(
          "JSON-Objekt ohne refreshSeconds angeben (maximal 8000 kodierte Bytes).",
          "Enter a JSON object without refreshSeconds (maximum 8000 encoded bytes).",
        ),
      );
    return { value, error: "" };
  } catch (error) {
    return {
      value: null,
      error: error instanceof Error ? error.message : String(error),
    };
  }
});
const imageUrl = computed(() => {
  if (!props.row || !imageToken.value) return "";
  const url = new URL("api/v1/dashboards/image.png", base);
  url.search = new URLSearchParams({
    node: props.node,
    id: props.row.id,
    token: imageToken.value,
  }).toString();
  return url.href;
});
const generateToken = () => {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  imageToken.value = Array.from(bytes, (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
};
const parsedSources = computed(() => {
  try {
    const value: unknown = JSON.parse(sources.value || "{}");
    return value && typeof value === "object" && !Array.isArray(value)
      ? { value: value as Dashboard["sources"], error: "" }
      : {
          value: null,
          error: tr("Ein JSON-Objekt angeben.", "Enter a JSON object."),
        };
  } catch (cause) {
    return {
      value: null,
      error: cause instanceof Error ? cause.message : String(cause),
    };
  }
});
const refreshError = computed(() => {
  const value = Number(refreshSeconds.value);
  return Number.isInteger(value) &&
    (value === 0 || (value >= 5 && value <= 86400))
    ? ""
    : tr("0 oder 5 bis 86400 Sekunden.", "0 or 5 to 86400 seconds.");
});
const valid = computed(
  () =>
    !!title.value.trim() &&
    !parsedSources.value.error &&
    !refreshError.value &&
    !parsedMetadata.value.error,
);
// The preview follows typing without rebuilding the sandbox on every keystroke.
const previewHtml = ref(html.value);
let previewTimer: ReturnType<typeof setTimeout> | undefined;
watch(html, (value) => {
  clearTimeout(previewTimer);
  previewTimer = setTimeout(() => (previewHtml.value = value), 600);
});
onBeforeUnmount(() => clearTimeout(previewTimer));
const submit = () => {
  if (!valid.value) return;
  emit("save", {
    title: title.value.trim(),
    html: html.value,
    sources: parsedSources.value.value!,
    refreshSeconds: Number(refreshSeconds.value),
    metadata: parsedMetadata.value.value!,
    imageToken: imageToken.value,
  });
};
</script>

<template>
  <ToolbarContent side="end">
    <Button v-if="row" variant="ghost" size="sm" as-child>
      <a
        :href="row.url"
        target="_blank"
        rel="noopener"
        :aria-label="tr('Rahmenlos öffnen', 'Open frameless')"
        ><ExternalLink aria-hidden="true" /><span class="hidden sm:inline">{{
          tr("Rahmenlos öffnen", "Open frameless")
        }}</span></a
      >
    </Button>
    <Button
      variant="outline"
      size="sm"
      :disabled="busy"
      @click="emit('cancel')"
      >{{ tr("Schließen", "Close") }}</Button
    >
    <Button size="sm" :disabled="busy || !valid" @click="submit"
      ><Save aria-hidden="true" />{{ tr("Speichern", "Save") }}</Button
    >
  </ToolbarContent>
  <form
    class="grid min-w-0 gap-6 xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]"
    @submit.prevent="submit"
  >
    <div class="min-w-0 space-y-5">
      <Field>
        <FieldLabel for="title">{{ tr("Titel", "Title") }}</FieldLabel>
        <Input
          id="title"
          v-model="title"
          required
          maxlength="200"
          :disabled="busy"
        />
      </Field>
      <Tabs default-value="html" class="min-w-0">
        <TabsList class="h-auto flex-wrap">
          <TabsTrigger value="html">HTML</TabsTrigger>
          <TabsTrigger value="sources">{{
            tr("Datenquellen", "Data sources")
          }}</TabsTrigger>
          <TabsTrigger value="refresh">{{
            tr("Aktualisierung", "Refresh")
          }}</TabsTrigger>
          <TabsTrigger value="settings">{{
            tr("Metadaten & PNG", "Metadata & PNG")
          }}</TabsTrigger>
        </TabsList>
        <TabsContent value="html" class="mt-3">
          <Field>
            <FieldLabel for="html" class="sr-only">HTML</FieldLabel>
            <Textarea
              id="html"
              wrap="off"
              v-model="html"
              class="min-h-[28rem] font-mono text-sm"
              spellcheck="false"
              :disabled="busy"
            />
            <FieldDescription
              >dashboard.onUpdate(async ({ data, output }) =&gt; { … }) ·
              output: mode, width, height, colorMode</FieldDescription
            >
          </Field>
        </TabsContent>
        <TabsContent value="sources" class="mt-3">
          <Field :data-invalid="!!parsedSources.error || undefined">
            <FieldLabel for="sources">{{
              tr("Hive-Datenquellen (JSON)", "Hive data sources (JSON)")
            }}</FieldLabel>
            <Textarea
              id="sources"
              wrap="off"
              v-model="sources"
              class="min-h-64 font-mono text-sm"
              spellcheck="false"
              :disabled="busy"
              :aria-invalid="!!parsedSources.error"
            />
            <FieldError v-if="parsedSources.error">{{
              parsedSources.error
            }}</FieldError>
            <FieldDescription v-else
              >{{
                tr(
                  "Jeder Name wird ein Feld in data. Beispiele",
                  "Each name becomes a field of data. Examples",
                )
              }}: {"weather":{"objectId":"…"}} ·
              {"tasks":{"contractKey":"task-board/task","limit":20}}</FieldDescription
            >
          </Field>
        </TabsContent>
        <TabsContent value="refresh" class="mt-3">
          <Field :data-invalid="!!refreshError || undefined" class="max-w-xs">
            <FieldLabel for="refresh">{{
              tr("Aktualisierung in Sekunden", "Refresh in seconds")
            }}</FieldLabel>
            <Input
              id="refresh"
              v-model="refreshSeconds"
              type="number"
              min="0"
              max="86400"
              :disabled="busy"
              :aria-invalid="!!refreshError"
            />
            <FieldError v-if="refreshError">{{ refreshError }}</FieldError>
            <FieldDescription v-else>{{
              tr(
                "0 = aus; mindestens 5 Sekunden.",
                "0 = off; minimum 5 seconds.",
              )
            }}</FieldDescription>
          </Field>
        </TabsContent>
        <TabsContent value="settings" class="mt-3 space-y-5">
          <Field :data-invalid="!!parsedMetadata.error || undefined">
            <FieldLabel for="metadata">{{
              tr("Metadaten (JSON)", "Metadata (JSON)")
            }}</FieldLabel>
            <Textarea
              id="metadata"
              v-model="metadata"
              class="min-h-40 font-mono text-sm"
              spellcheck="false"
              :disabled="busy"
              :aria-invalid="!!parsedMetadata.error"
            />
            <FieldError v-if="parsedMetadata.error">{{
              parsedMetadata.error
            }}</FieldError>
            <FieldDescription v-else>{{
              tr(
                "Zusätzliche Metadaten für Displays. refreshSeconds wird aus der Aktualisierungseinstellung ergänzt.",
                "Additional display metadata. refreshSeconds is included from the refresh setting.",
              )
            }}</FieldDescription>
          </Field>
          <Field>
            <FieldLabel>{{ tr("PNG-Zugriff", "PNG access") }}</FieldLabel>
            <FieldDescription>{{
              tr(
                "Ein eigener Token erlaubt den direkten Bildabruf ohne Hive-Anmeldung. Änderungen werden beim Speichern wirksam.",
                "A dedicated token allows direct image retrieval without Hive sign-in. Save to apply changes.",
              )
            }}</FieldDescription>
            <div class="flex flex-wrap gap-2">
              <Button
                type="button"
                variant="outline"
                :disabled="busy"
                @click="generateToken"
                >{{
                  imageToken
                    ? tr("Token erneuern", "Rotate token")
                    : tr("PNG-Zugriff aktivieren", "Enable PNG access")
                }}</Button
              >
              <Button
                v-if="imageToken"
                type="button"
                variant="outline"
                :disabled="busy"
                @click="imageToken = null"
                >{{ tr("Deaktivieren", "Disable") }}</Button
              >
            </div>
          </Field>
          <Field v-if="imageUrl">
            <FieldLabel for="image-url">{{
              tr("PNG-URL", "PNG URL")
            }}</FieldLabel>
            <Textarea
              id="image-url"
              :model-value="imageUrl"
              readonly
              class="min-h-24 break-all font-mono text-xs"
            />
            <FieldDescription>{{
              tr(
                "Diese URL enthält den Zugriffstoken. Für Displays können width, height und colorMode ergänzt werden.",
                "This URL contains the access token. Displays can add width, height and colorMode.",
              )
            }}</FieldDescription>
          </Field>
        </TabsContent>
      </Tabs>
    </div>
    <Card
      class="min-w-0 gap-3 overflow-hidden py-0 xl:sticky xl:top-16 xl:self-start"
    >
      <CardHeader class="border-b py-3">
        <CardTitle class="text-sm">{{ tr("Vorschau", "Preview") }}</CardTitle>
        <CardDescription>{{
          row
            ? tr(
                "Entwurf mit den Daten der gespeicherten Quellen.",
                "Draft with data from the saved sources.",
              )
            : tr(
                "Daten erscheinen nach dem ersten Speichern.",
                "Data appears after the first save.",
              )
        }}</CardDescription>
      </CardHeader>
      <CardContent class="h-[26rem] p-0 xl:h-[calc(100dvh-14rem)]">
        <Viewer
          :node="node"
          :html="previewHtml"
          v-bind="row ? { id: row.id } : {}"
        />
      </CardContent>
    </Card>
  </form>
</template>
