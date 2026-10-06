<script setup lang="ts">
import { ref } from "vue";
import {
  Download,
  ExternalLink,
  MoreHorizontal,
  Pencil,
  Trash2,
} from "@lucide/vue";
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Field,
  FieldLabel,
  Input,
  OptionSelect,
  ToolbarContent,
} from "@ivy/ui";
import { call, tr } from "./runtime";
import type { Row } from "./runtime";
import Viewer from "./Viewer.vue";

const props = defineProps<{ node: string; row: Row; busy: boolean }>();
const emit = defineEmits<{ edit: []; remove: [] }>();
const exporting = ref(false),
  deleting = ref(false),
  imageBusy = ref(false),
  imageError = ref("");
const width = ref(800),
  height = ref(600),
  colorMode = ref("color");
async function download() {
  imageBusy.value = true;
  imageError.value = "";
  try {
    const result = await call<{ data: string }>(props.node, "render", {
      id: props.row.id,
      width: Number(width.value),
      height: Number(height.value),
      colorMode: colorMode.value,
    });
    const url = URL.createObjectURL(
      new Blob([Uint8Array.from(atob(result.data), (c) => c.charCodeAt(0))], {
        type: "image/png",
      }),
    );
    const link = document.createElement("a");
    link.href = url;
    link.download = (props.row.value.title || "dashboard") + ".png";
    link.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    exporting.value = false;
  } catch (cause) {
    imageError.value = cause instanceof Error ? cause.message : String(cause);
  } finally {
    imageBusy.value = false;
  }
}
</script>

<template>
  <ToolbarContent side="end">
    <Button variant="ghost" size="sm" as-child>
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
    <Button variant="outline" size="sm" :disabled="busy" @click="emit('edit')"
      ><Pencil aria-hidden="true" />{{ tr("Bearbeiten", "Edit") }}</Button
    >
    <DropdownMenu>
      <DropdownMenuTrigger as-child>
        <Button
          variant="ghost"
          size="icon-sm"
          :aria-label="tr('Weitere Aktionen', 'More actions')"
          ><MoreHorizontal aria-hidden="true"
        /></Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" class="w-52">
        <DropdownMenuItem @select="exporting = true"
          ><Download aria-hidden="true" />{{
            tr("Als PNG exportieren", "Export as PNG")
          }}</DropdownMenuItem
        >
        <DropdownMenuSeparator />
        <DropdownMenuItem
          variant="destructive"
          :disabled="busy"
          @select="deleting = true"
          ><Trash2 aria-hidden="true" />{{
            tr("Löschen", "Delete")
          }}</DropdownMenuItem
        >
      </DropdownMenuContent>
    </DropdownMenu>
  </ToolbarContent>
  <div
    class="h-[calc(100dvh-6.5rem)] min-h-80 overflow-hidden rounded-xl border md:h-[calc(100dvh-7.5rem)]"
  >
    <Viewer :key="row.id" :node="node" :id="row.id" />
  </div>

  <Dialog v-model:open="exporting">
    <DialogContent class="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>{{
          tr("Als PNG exportieren", "Export as PNG")
        }}</DialogTitle>
        <DialogDescription>{{
          tr(
            "Rendert die gespeicherte Fassung mit aktuellen Daten.",
            "Renders the saved version with current data.",
          )
        }}</DialogDescription>
      </DialogHeader>
      <div class="grid grid-cols-2 gap-4">
        <Field
          ><FieldLabel for="png-width">{{ tr("Breite", "Width") }}</FieldLabel
          ><Input
            id="png-width"
            v-model="width"
            type="number"
            min="100"
            max="4096"
        /></Field>
        <Field
          ><FieldLabel for="png-height">{{ tr("Höhe", "Height") }}</FieldLabel
          ><Input
            id="png-height"
            v-model="height"
            type="number"
            min="100"
            max="4096"
        /></Field>
        <Field class="col-span-2">
          <FieldLabel for="png-color">{{
            tr("Farbmodus", "Color mode")
          }}</FieldLabel>
          <OptionSelect id="png-color" v-model="colorMode" class="w-full">
            <option value="color">{{ tr("Farbe", "Color") }}</option>
            <option value="grayscale">
              {{ tr("Graustufen", "Grayscale") }}
            </option>
            <option value="monochrome">
              {{ tr("Schwarz/Weiß", "Black/white") }}
            </option>
          </OptionSelect>
        </Field>
      </div>
      <p v-if="imageError" role="alert" class="text-sm text-destructive">
        {{ imageError }}
      </p>
      <DialogFooter>
        <Button variant="outline" @click="exporting = false">{{
          tr("Abbrechen", "Cancel")
        }}</Button>
        <Button :disabled="imageBusy" @click="download"
          ><Download aria-hidden="true" />{{
            imageBusy
              ? tr("Wird gerendert …", "Rendering…")
              : tr("PNG herunterladen", "Download PNG")
          }}</Button
        >
      </DialogFooter>
    </DialogContent>
  </Dialog>

  <Dialog v-model:open="deleting">
    <DialogContent class="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>{{
          tr("Dashboard löschen?", "Delete dashboard?")
        }}</DialogTitle>
        <DialogDescription>{{
          tr(
            `„${row.value.title}“ wird dauerhaft gelöscht. Rahmenlose Links funktionieren danach nicht mehr.`,
            `“${row.value.title}” is deleted permanently. Frameless links stop working.`,
          )
        }}</DialogDescription>
      </DialogHeader>
      <DialogFooter>
        <Button variant="outline" @click="deleting = false">{{
          tr("Abbrechen", "Cancel")
        }}</Button>
        <Button
          variant="destructive"
          :disabled="busy"
          @click="
            deleting = false;
            emit('remove');
          "
          ><Trash2 aria-hidden="true" />{{ tr("Löschen", "Delete") }}</Button
        >
      </DialogFooter>
    </DialogContent>
  </Dialog>
</template>
