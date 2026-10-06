<script setup lang="ts">
import { computed, onMounted, ref, useSlots, watch } from "vue";
import { useElementSize, useMediaQuery } from "@vueuse/core";
import type { Component } from "vue";
import {
  Bell,
  CircleUser,
  LogOut,
  Moon,
  PanelLeft,
  Server,
  Settings2,
  Sun,
} from "@lucide/vue";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "./components/dropdown-menu";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
} from "./components/sidebar";
import { Tooltip, TooltipContent, TooltipTrigger } from "./components/tooltip";
import { Button } from "./components/button";
import RailTooltip from "./patterns/RailTooltip.vue";
import ShellTrigger from "./patterns/ShellTrigger.vue";
import StatusBadge from "./patterns/StatusBadge.vue";
import IvyLogo from "./IvyLogo.vue";
import IvyMark from "./IvyMark.vue";
import BrowserAppDialog from "./patterns/BrowserAppDialog.vue";
import InstallAppBanner from "./patterns/InstallAppBanner.vue";
import type { BrowserAppControls } from "./lib/browser-app";

export interface ShellNavigationItem {
  label: string;
  href: string;
  active: boolean;
  group?: string;
  icon?: Component;
}
/** An installed UI in the permanent app rail. */
export interface ShellApp {
  id: string;
  name: string;
  href: string;
  icon: Component;
}
const props = defineProps<{
  uiName: string;
  homeUrl: string;
  /** Installed UIs; the rail marks `currentApp` ("" is the UI list, "system" is System). */
  apps?: ShellApp[] | undefined;
  currentApp?: string | undefined;
  systemHref?: string | undefined;
  /** Settings of the current UI, shown at the right end of the top bar. */
  settingsHref?: string | undefined;
  settingsLabel?: string | undefined;
  logoutUrl: string;
  browserApp?: BrowserAppControls;
  layout?: "default" | "conversation" | undefined;
  navigation: ShellNavigationItem[];
  connection: string;
  connectionTone?: "good" | "bad" | "warning" | "neutral";
}>();
const rail = computed(() => [
  ...(props.apps ?? []).map((app) => ({
    ...app,
    active: props.currentApp === app.id,
  })),
  ...(props.systemHref
    ? [
        {
          id: "system",
          name: "System",
          href: props.systemHref,
          icon: Server,
          active: props.currentApp === "system",
        },
      ]
    : []),
]);
const dark = ref(false),
  browserAppOpen = ref(false),
  sidebarOpen = ref(true),
  logout = ref<HTMLFormElement>();
const installBanner = ref<HTMLElement>();
const { height: installBannerHeight } = useElementSize(installBanner);
const sidebarKey = computed(() => `ivy.sidebar.collapsed:${props.uiName}`);
// Read the collapsed state before the first render so the layout does not jump.
try {
  sidebarOpen.value = localStorage.getItem(sidebarKey.value) !== "true";
} catch {
  /* Optional view state. */
}
const slots = useSlots();
// UIs without page navigation get only the app rail, not an empty page menu. The mobile
// sheet has no hover tooltips, so there the rail fills the sheet and names its entries.
const mobile = useMediaQuery("(max-width: 767px)");
const panel = computed(
  () =>
    props.navigation.length > 0 ||
    !!(slots.sidebar || slots["sidebar-actions"] || slots["sidebar-footer"]),
);
const named = computed(() => !panel.value && mobile.value);
const groups = computed(() => {
  const result: Array<{ label: string; items: ShellNavigationItem[] }> = [];
  for (const item of props.navigation) {
    const label = item.group ?? "";
    if (result.at(-1)?.label === label) result.at(-1)!.items.push(item);
    else result.push({ label, items: [item] });
  }
  return result;
});
const skipToContent = () => document.getElementById("main-content")?.focus();
const applyTheme = () =>
  document.documentElement.classList.toggle("dark", dark.value);
const toggleTheme = () => {
  dark.value = !dark.value;
  applyTheme();
  try {
    localStorage.setItem("ivy.theme", dark.value ? "dark" : "light");
  } catch {
    /* Theme storage is optional. */
  }
};
watch(sidebarOpen, (open) => {
  try {
    if (open) localStorage.removeItem(sidebarKey.value);
    else localStorage.setItem(sidebarKey.value, "true");
  } catch {
    /* Optional view state. */
  }
});
onMounted(() => {
  try {
    const value = localStorage.getItem("ivy.theme");
    dark.value = value
      ? value === "dark"
      : matchMedia("(prefers-color-scheme: dark)").matches;
  } catch {
    dark.value = matchMedia("(prefers-color-scheme: dark)").matches;
  }
  applyTheme();
});
</script>
<template>
  <a
    href="#main-content"
    class="sr-only z-[100] rounded-md bg-primary p-3 text-primary-foreground focus:not-sr-only focus:fixed focus:left-4 focus:top-4"
    @click.prevent="skipToContent"
    >Skip to content</a
  >
  <SidebarProvider
    v-slot="{ setOpenMobile }"
    v-model:open="sidebarOpen"
    :style="{
      '--ivy-install-banner-height': installBannerHeight + 'px',
      '--sidebar-width': panel
        ? 'calc(16rem + var(--sidebar-width-icon) + 1px)'
        : 'calc(var(--sidebar-width-icon) + 1px)',
    }"
    :data-ui="uiName"
    :data-layout="layout ?? 'default'"
    class="ivy-shell"
  >
    <!-- shadcn sidebar-09: a permanent app rail beside the collapsible page navigation. -->
    <Sidebar id="ivy-navigation" collapsible="icon" class="overflow-hidden">
      <div class="flex h-full w-full min-w-0">
        <Sidebar
          collapsible="none"
:class="
            named
              ? 'ivy-rail w-full! border-r'
              : 'ivy-rail w-[calc(var(--sidebar-width-icon)+1px)]! border-r'
          "
          aria-label="Apps"
        >
          <SidebarHeader>
            <SidebarMenu>
              <SidebarMenuItem>
                <RailTooltip label="All UIs">
                  <SidebarMenuButton
                    as-child
                    :is-active="currentApp === ''"
                    :class="named ? 'p-1.5' : 'justify-center p-1.5'"
                  >
                    <a
                      :href="homeUrl"
                      aria-label="Ivy home"
                      :aria-current="currentApp === '' ? 'page' : undefined"
                      ><IvyMark class="size-5!" /><span v-if="named"
                        >All UIs</span
                      ></a
                    >
                  </SidebarMenuButton>
                </RailTooltip>
              </SidebarMenuItem>
            </SidebarMenu>
          </SidebarHeader>
          <SidebarContent>
            <SidebarGroup class="px-2 py-0">
              <SidebarGroupContent>
                <SidebarMenu>
                  <SidebarMenuItem
                    v-for="item in rail"
                    :key="item.id"
                    :class="{
                      'mt-1 border-t border-sidebar-border pt-1':
                        item.id === 'system' && rail.length > 1,
                    }"
                  >
                    <RailTooltip :label="item.name">
                      <SidebarMenuButton
                        as-child
                        :is-active="item.active"
                        :class="named ? '' : 'justify-center'"
                      >
                        <a
                          :href="item.href"
                          :aria-label="item.name"
                          :aria-current="item.active ? 'page' : undefined"
                          ><component :is="item.icon" aria-hidden="true" /><span
                            v-if="named"
                            >{{ item.name }}</span
                          ></a
                        >
                      </SidebarMenuButton>
                    </RailTooltip>
                  </SidebarMenuItem>
                </SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          </SidebarContent>
          <SidebarFooter>
            <SidebarMenu>
              <SidebarMenuItem>
                <DropdownMenu>
                  <DropdownMenuTrigger as-child>
                    <SidebarMenuButton
                      aria-label="Account"
                      :class="named ? '' : 'justify-center'"
                    >
                      <CircleUser aria-hidden="true" /><span v-if="named"
                        >Account</span
                      >
                    </SidebarMenuButton>
                  </DropdownMenuTrigger>
                  <DropdownMenuContent side="right" align="end" class="w-56">
                    <DropdownMenuItem v-if="browserApp" @select="setOpenMobile(false); browserAppOpen = true"><Bell aria-hidden="true" />App &amp; notifications</DropdownMenuItem>
                    <DropdownMenuItem @select="toggleTheme"
                      ><Sun v-if="dark" aria-hidden="true" /><Moon
                        v-else
                        aria-hidden="true"
                      />{{
                        dark ? "Use light theme" : "Use dark theme"
                      }}</DropdownMenuItem
                    >
                    <DropdownMenuSeparator />
                    <DropdownMenuItem @select="logout?.submit()"
                      ><LogOut aria-hidden="true" />Sign out</DropdownMenuItem
                    >
                  </DropdownMenuContent>
                </DropdownMenu>
                <form ref="logout" :action="logoutUrl" method="post" hidden />
              </SidebarMenuItem>
            </SidebarMenu>
          </SidebarFooter>
        </Sidebar>
        <Sidebar v-if="panel" collapsible="none" class="min-w-0 flex-1">
          <SidebarHeader>
            <p
              class="flex h-8 items-center truncate px-2 text-sm font-semibold"
            >
              {{ uiName }}
            </p>
          </SidebarHeader>
          <SidebarContent>
            <slot name="sidebar-actions" />
            <nav v-if="navigation.length" aria-label="Page navigation">
              <SidebarGroup v-for="group in groups" :key="group.label">
                <SidebarGroupLabel v-if="group.label">{{
                  group.label
                }}</SidebarGroupLabel>
                <SidebarGroupContent>
                  <SidebarMenu>
                    <SidebarMenuItem
                      v-for="item in group.items"
                      :key="item.href"
                    >
                      <SidebarMenuButton :is-active="item.active" as-child>
                        <a
                          :href="item.href"
                          :aria-current="item.active ? 'page' : undefined"
                          ><component
                            :is="item.icon"
                            v-if="item.icon"
                            aria-hidden="true"
                          /><span>{{ item.label }}</span></a
                        >
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  </SidebarMenu>
                </SidebarGroupContent>
              </SidebarGroup>
            </nav>
            <slot name="sidebar" />
          </SidebarContent>
          <SidebarFooter v-if="$slots['sidebar-footer']">
            <slot name="sidebar-footer" />
          </SidebarFooter>
        </Sidebar>
      </div>
    </Sidebar>
    <SidebarInset class="min-w-0">
      <header class="ivy-toolbar">
        <ShellTrigger :class="{ 'md:hidden': !panel }">
          <PanelLeft aria-hidden="true" />
        </ShellTrigger>
        <a
          :href="homeUrl"
          aria-label="Ivy home"
          class="ivy-toolbar-logo shrink-0 items-center rounded-md text-foreground focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-hidden"
        >
          <IvyLogo class="h-4 w-11" />
        </a>
        <div
          id="ivy-toolbar-start"
          class="flex min-w-0 flex-1 items-center gap-2"
        >
          <slot name="context" />
        </div>
        <div id="ivy-toolbar-end" class="flex shrink-0 items-center gap-1">
          <StatusBadge
            v-if="connectionTone === 'bad' || connectionTone === 'warning'"
            :label="connection"
            :tone="connectionTone"
          />
        </div>
        <Tooltip v-if="settingsHref">
          <TooltipTrigger as-child>
            <Button variant="ghost" size="icon-sm" as-child>
              <a :href="settingsHref" :aria-label="settingsLabel ?? 'Settings'">
                <Settings2 aria-hidden="true" />
              </a>
            </Button>
          </TooltipTrigger>
          <TooltipContent>{{ settingsLabel ?? 'Settings' }}</TooltipContent>
        </Tooltip>
      </header>
      <div v-if="browserApp" ref="installBanner" class="shrink-0">
        <InstallAppBanner :app="browserApp" @help="browserAppOpen = true" />
      </div>
      <div id="main-content" tabindex="-1" class="ivy-main"><slot /></div>
    </SidebarInset>
  </SidebarProvider>
  <BrowserAppDialog v-if="browserApp" v-model:open="browserAppOpen" :app="browserApp" />
</template>
