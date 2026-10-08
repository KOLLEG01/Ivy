<script setup lang="ts">
import { computed } from "vue";
import {
  Alert,
  AlertDescription,
  Button,
  IvyShell,
  uiIcon,
  useRemote,
} from "@ivy/ui";
import type { ShellNavigationItem } from "@ivy/ui";
import type { HiveClient } from "../../sdk/src/client.js";
import { uiPath } from "../../contracts/src/ui-route.js";
import { useBrowserApp } from "./browser-app";
const props = defineProps<{
  uiName: string;
  base: URL;
  client: HiveClient;
  layout?: "default" | "conversation";
  settingsHref?: string;
  settingsLabel?: string;
  navigation: ShellNavigationItem[];
}>();
const status = useRemote(
  (signal) => props.client.request("system.status", {}, { signal }),
  30000,
  ["system/runtime"],
);
// The rail lists every installed UI; this page is the one whose URL is open.
const catalog = useRemote(
  (signal) => props.client.request("uis.catalog", { limit: 50 }, { signal }),
  0,
  ["uis"],
);
const apps = computed(() =>
  (catalog.value.value?.items ?? [])
    .filter((ui) => ui.currentReleaseId && ui.metadata.uiId !== "chat-ui")
    .map((ui) => ({
      id: ui.metadata.uiId,
      name: ui.metadata.displayName,
      icon: uiIcon(ui.metadata.iconKey),
      href: new URL(
        uiPath(ui.metadata),
        props.base,
      ).href,
    })),
);
const currentApp = computed(() => catalog.value.value?.items.find(ui => {
  const current = location.pathname.slice(props.base.pathname.length);
  return current.startsWith(uiPath(ui.metadata)) || current.startsWith('ui/' + encodeURIComponent(ui.metadata.uiId) + '/');
})?.metadata.uiId);
const systemHref = new URL("#/system/nodes", props.base).href;
const login = () =>
  new URL(
    "login?returnTo=" + encodeURIComponent(location.pathname + location.hash),
    props.base,
  ).href;
const logoutUrl = new URL("logout", props.base).href;
const browserApp = useBrowserApp(props.base);
</script>
<template>
  <IvyShell
    :ui-name="uiName"
    :browser-app="browserApp"
    :layout="layout"
    :settings-href="settingsHref"
    :settings-label="settingsLabel"
    :home-url="base.href"
    :apps="apps"
    :current-app="currentApp"
    :system-href="systemHref"
    :logout-url="logoutUrl"
    :navigation="navigation"
    :connection="
      status.error.value
        ? 'Connection unavailable'
        : status.value.value?.ready
          ? 'Hive ready'
          : 'Connecting'
    "
    :connection-tone="
      status.error.value
        ? 'warning'
        : status.value.value?.ready
          ? 'good'
          : 'neutral'
    "
  >
    <template v-if="$slots['sidebar-actions']" #sidebar-actions
      ><slot name="sidebar-actions"
    /></template>
    <template v-if="$slots.sidebar" #sidebar><slot name="sidebar" /></template>
    <template v-if="$slots['sidebar-footer']" #sidebar-footer
      ><slot name="sidebar-footer"
    /></template>
    <template v-if="$slots.context" #context><slot name="context" /></template>
    <Alert v-if="status.error.value" variant="destructive" class="mb-4"
      ><AlertDescription
        class="flex flex-wrap items-center justify-between gap-3"
        ><span>{{ status.error.value }}</span
        ><Button variant="outline" size="sm" as-child
          ><a :href="login()">Sign in</a></Button
        ></AlertDescription
      ></Alert
    >
    <slot />
  </IvyShell>
</template>
