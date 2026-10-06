<script setup lang="ts">
import type { BrowserAppControls } from "../lib/browser-app";
import { watch } from "vue";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "../components/dialog";
import { Button } from "../components/button";
import { Alert, AlertDescription } from "../components/alert";
const props = defineProps<{ app: BrowserAppControls }>();
const open = defineModel<boolean>("open", { default: false });
watch(open, (value) => {
  if (value) void props.app.refresh();
});
</script>
<template>
  <Dialog v-model:open="open">
    <DialogContent class="sm:max-w-md">
      <DialogHeader>
        <DialogTitle>App &amp; notifications</DialogTitle>
        <DialogDescription
          >Install Ivy and manage notifications on this
          device.</DialogDescription
        >
      </DialogHeader>
      <section class="space-y-3" aria-labelledby="browser-app-install">
        <h2 id="browser-app-install" class="text-sm font-medium">
          Install Ivy
        </h2>
        <p v-if="app.installed" class="text-sm text-muted-foreground">
          Ivy is running as an installed app.
        </p>
        <Button
          v-else-if="app.installAvailable"
          :disabled="app.busy"
          @click="app.install()"
          >Install app</Button
        >
        <p v-else class="text-sm text-muted-foreground">
          Use your browser menu to install Ivy. On iPhone or iPad, use Share →
          Add to Home Screen. On Safari for Mac, use File → Add to Dock.
        </p>
      </section>
      <section
        class="space-y-3 border-t pt-4"
        aria-labelledby="browser-app-push"
      >
        <h2 id="browser-app-push" class="text-sm font-medium">Notifications</h2>
        <p class="text-sm text-muted-foreground">
          Get agent results, input requests, task reviews and Secretary notices,
          even when the Ivy tab is closed.
        </p>
        <p v-if="!app.supported" class="text-sm text-muted-foreground">
          This browser cannot receive push notifications here. Use HTTPS and a
          browser with Web Push support. On iPhone or iPad, open Ivy from the
          Home Screen first.
        </p>
        <p
          v-else-if="app.permission === 'denied'"
          class="text-sm text-muted-foreground"
        >
          Notifications are blocked. Allow them in your browser or device
          settings, then reopen this panel.
        </p>
        <div v-else class="flex flex-wrap gap-2">
          <Button
            v-if="!app.enabled"
            :disabled="app.busy || !app.ready"
            @click="app.enable()"
            >Enable notifications</Button
          >
          <template v-else>
            <Button
              variant="outline"
              :disabled="app.busy"
              @click="app.disable()"
              >Disable notifications</Button
            >
            <Button variant="outline" :disabled="app.busy" @click="app.test()"
              >Send test notification</Button
            >
          </template>
        </div>
        <p
          v-if="app.supported"
          class="text-sm text-muted-foreground"
          role="status"
        >
          {{
            app.busy
              ? "Updating…"
              : app.enabled
                ? "Notifications are enabled on this browser."
                : "Notifications are off on this browser."
          }}
        </p>
      </section>
      <Alert v-if="app.error" variant="destructive"
        ><AlertDescription
          >{{ app.error }}
          <Button
            variant="link"
            size="sm"
            :disabled="app.busy"
            @click="app.refresh()"
            >Retry</Button
          ></AlertDescription
        ></Alert
      >
      <p v-if="app.message" class="text-sm text-muted-foreground" role="status">
        {{ app.message }}
      </p>
    </DialogContent>
  </Dialog>
</template>
