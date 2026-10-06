<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from "vue";
import { dashboardDocument } from "../../../services/dashboards/src/document";
import { base, call, tr } from "./runtime";
import type { Row } from "./runtime";
// `html` previews an unsaved draft; with an `id` it renders against that dashboard's live data.
const props = defineProps<{
  node: string;
  id?: string;
  html?: string;
  colorMode?: string;
}>();
const frame = ref<HTMLIFrameElement>(),
  source = ref(""),
  error = ref("");
const frameUrl = new URL("dashboards/dashboard.sandbox.html", base).href;
const frameVersion = ref(0);
const channel = crypto.randomUUID();
const current = ref<Row | null>(null);
let data: unknown = {},
  ready = false,
  sandboxReady = false,
  stopped = false,
  timer: ReturnType<typeof setTimeout> | undefined,
  sequence = 0,
  rendered = false,
  renderedHtml: string | null = null,
  renderedRevision = 0;
const render = (html: string) => {
  ready = false;
  renderedHtml = html;
  source.value = dashboardDocument(html, channel);
  if (rendered) {
    sandboxReady = false;
    frameVersion.value++;
  } else sendDocument();
  rendered = true;
};
const sendDocument = () => {
  if (sandboxReady && source.value) {
    sandboxReady = false;
    frame.value?.contentWindow?.postMessage(
      { type: "document", html: source.value },
      "*",
    );
  }
};
let resize: ResizeObserver | undefined;
const send = () => {
  if (!ready || !frame.value) return;
  const { width, height } = frame.value.getBoundingClientRect();
  frame.value.contentWindow?.postMessage(
    {
      channel,
      type: "update",
      sequence: ++sequence,
      data,
      output: {
        mode: "interactive",
        width: Math.round(width),
        height: Math.round(height),
        colorMode: props.colorMode ?? "color",
      },
    },
    "*",
  );
};
async function refresh() {
  clearTimeout(timer);
  try {
    if (props.id) {
      const result = await call<{ data: unknown; dashboard: Row }>(
        props.node,
        "data",
        { id: props.id },
      );
      if (stopped) return;
      const row = result.dashboard,
        html = props.html ?? row.value.html;
      data = result.data;
      if (row.revision !== renderedRevision || html !== renderedHtml) {
        renderedRevision = row.revision;
        render(html);
      }
      current.value = row;
    } else if (props.html !== renderedHtml) render(props.html ?? "");
    error.value = "";
    send();
  } catch (cause) {
    if (!stopped) error.value = String(cause);
  } finally {
    if (!stopped && props.id && (current.value?.value.refreshSeconds ?? 5) > 0)
      timer = setTimeout(
        refresh,
        (current.value?.value.refreshSeconds ?? 5) * 1000,
      );
  }
}
watch(
  () => props.html,
  (html) => {
    if (html !== undefined && html !== renderedHtml) render(html);
  },
);
const receive = (event: MessageEvent) => {
  if (event.source !== frame.value?.contentWindow) return;
  if (event.data?.type === "sandbox-ready") {
    sandboxReady = true;
    sendDocument();
    return;
  }
  if (event.data?.channel !== channel) return;
  if (event.data.type === "ready") {
    ready = true;
    send();
  }
  if (event.data.type === "error" && event.data.sequence === sequence)
    error.value = event.data.message;
};
onMounted(() => {
  window.addEventListener("message", receive);
  resize = new ResizeObserver(send);
  if (frame.value) resize.observe(frame.value);
  void refresh();
});
watch(frame, (value) => {
  resize?.disconnect();
  if (value) resize?.observe(value);
});
onBeforeUnmount(() => {
  stopped = true;
  clearTimeout(timer);
  resize?.disconnect();
  window.removeEventListener("message", receive);
});
</script>
<template>
  <div class="relative h-full w-full">
    <p
      v-if="error"
      role="alert"
      class="absolute inset-x-0 top-0 z-10 bg-destructive p-3 text-destructive-foreground"
    >
      {{ error }}
    </p>
    <p v-else-if="!source" role="status" class="p-4">
      {{ tr("Dashboard wird geladen …", "Loading dashboard…") }}
    </p>
    <iframe
      :key="frameVersion"
      ref="frame"
      :src="frameUrl"
      :title="current?.value.title ?? 'Dashboard'"
      sandbox="allow-scripts"
      referrerpolicy="no-referrer"
      class="block h-full w-full border-0 bg-white"
    />
  </div>
</template>
