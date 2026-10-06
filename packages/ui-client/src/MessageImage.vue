<script setup lang="ts">
import { watch } from "vue";
import { RemoteState, useRemote } from "@ivy/ui";
import type { RpcClient } from "../../sdk/src/client.js";
import { nativeRead, record, text } from "./native";
import type { MessageImage } from "./message-images";

const props = defineProps<{
  client: RpcClient;
  node: string;
  image: MessageImage;
}>();
const source = useRemote(async (signal) => {
  if (props.image.url) return props.image.url;
  const file = record(
    await nativeRead(
      props.client,
      props.node,
      "codex.fs/readFile",
      { path: props.image.path },
      signal,
    ),
  );
  const bytes = text(file.dataBase64);
  const mime = bytes.startsWith("iVBORw0KGgo")
    ? "image/png"
    : bytes.startsWith("/9j/")
      ? "image/jpeg"
      : bytes.startsWith("R0lGOD")
        ? "image/gif"
        : bytes.startsWith("UklGR")
          ? "image/webp"
          : null;
  if (!mime) throw new Error("The attachment is not a supported image.");
  return `data:${mime};base64,${bytes}`;
});
watch(
  () => [props.node, props.image.path, props.image.url],
  () => source.refresh(),
);
</script>
<template>
  <figure class="min-w-0">
    <RemoteState
      :loading="source.loading.value"
      :error="source.error.value"
      :has-data="!!source.value.value"
      @retry="source.refresh"
    />
    <img
      v-if="source.value.value"
      :src="source.value.value"
      :alt="image.name"
      class="max-h-80 max-w-full rounded-md object-contain"
      loading="lazy"
    />
  </figure>
</template>
