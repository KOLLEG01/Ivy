<script setup lang="ts">
import type { RpcClient } from "../../sdk/src/client.js";
import type { MessageImage as Image } from "./message-images";
import MessageImage from "./MessageImage.vue";
defineProps<{ client: RpcClient; node: string; images: Image[] }>();
</script>
<template>
  <div class="mb-3 flex min-w-0 flex-wrap gap-3">
    <MessageImage
      v-for="(image, index) in images"
      :key="index"
      :client="client"
      :node="node"
      :image="image"
    />
  </div>
</template>
