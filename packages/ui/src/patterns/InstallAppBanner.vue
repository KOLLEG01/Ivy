<script setup lang="ts">
import { X } from "@lucide/vue";
import type { BrowserAppControls } from "../lib/browser-app";
import { Alert, AlertDescription, AlertTitle } from "../components/alert";
import { Button } from "../components/button";
import IvyMark from "../IvyMark.vue";

const props = defineProps<{ app: BrowserAppControls }>();
const emit = defineEmits<{ help: [] }>();
async function install() {
  if (!props.app.installAvailable) {
    emit("help");
    return;
  }
  await props.app.install();
  if (props.app.error) emit("help");
}
</script>
<template>
  <Alert
    v-if="app.installBannerVisible"
    role="region"
    aria-label="Install Ivy"
    class="flex items-start gap-3 rounded-none border-x-0 border-t-0"
  >
    <IvyMark class="shrink-0" aria-hidden="true" />
    <div
      class="min-w-0 flex-1 space-y-2 sm:flex sm:items-center sm:justify-between sm:gap-4 sm:space-y-0"
    >
      <div class="space-y-1">
        <AlertTitle>Install Ivy</AlertTitle>
        <AlertDescription
          >Open Ivy from your home screen or desktop.</AlertDescription
        >
      </div>
      <Button size="sm" :disabled="app.busy" @click="install">
        {{ app.installAvailable ? "Install app" : "How to install" }}
      </Button>
    </div>
    <Button
      variant="ghost"
      size="icon-sm"
      aria-label="Dismiss installation banner"
      @click="app.dismissInstallBanner()"
      ><X aria-hidden="true"
    /></Button>
  </Alert>
</template>
